//! Record sets: many documents printed one after another as one PDF file, with one of them
//! in memory at a time.

use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use rustyprint_dom::budget;
use rustyprint_layout::ImageCache;
use rustyprint_layout::display::{ColorProfile, Document};
use rustyprint_pdf::{Session, Setup};

use crate::{
    Error, Limit, Limits, Preceding, RenderOptions, STACK_SIZE, budget_of, check_input_size,
    check_scale, exceeded, lay_out_after, limit_error, panic_message,
};

/// The decoded pixels a set keeps, at the most, of the images that its records share.
const SHARED_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// A record added to a [`RecordSet`].
#[derive(Debug, Clone, PartialEq)]
pub struct Added {
    /// How many pages the record took.
    pub pages: usize,
    /// Problems found in the record's CSS, as `source:line: message`, and problems loading
    /// its images.
    pub warnings: Vec<String>,
}

/// A finished [`RecordSet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// How many pages the file has.
    pub pages: usize,
    /// How many records were added.
    pub records: usize,
}

/// Many documents printed as one PDF file, one record at a time: each record is parsed,
/// laid out and written to the writer by [`RecordSet::add`], and then let go of, so the set
/// holds one record however many it prints.
///
/// ```no_run
/// let file = std::io::BufWriter::new(std::fs::File::create("statements.pdf")?);
/// let mut set = rustyprint::RecordSet::open(file, &rustyprint::RenderOptions::default())?;
/// for name in ["Ann", "Ben"] {
///     set.add(&format!("<h1>Statement for {name}</h1>"))?;
/// }
/// let (_file, summary) = set.finish()?;
/// assert_eq!(summary.records, 2);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// Every record is a document of its own with the options of the set: its stylesheet,
/// assets, page size, margins and scale.
///
/// - Every record starts on a new page, and its first page is a `:first` page.
/// - Page numbers run through the set: `counter(page)` on the first page of a record is
///   one more than the pages before it, and left and right pages carry on as well.
/// - `counter(pages)` is the number of pages of the record, not of the set.
/// - Links and `target-counter()` reach inside a record only: a link to an element of
///   another record does not resolve.
/// - The file's title is [`RenderOptions::title`], or else the `<title>` of the first
///   record. Its color profiles are those of the first record, and a record with other
///   `@color-profile` rules fails.
/// - Fonts are loaded once and embedded once. An image is written to the file once
///   however many records use it. Decoded images are kept between records only once a
///   second record uses the same file, up to 64 MB of pixels with the ones used the
///   longest ago let go first, so a logo is decoded twice and a record's own photo is
///   freed with the record.
///
/// [`Limits`] hold for each record, with the deadline counted from the start of its
/// `add`; the whole set has [`Limits::max_total_pages`] pages at the most.
///
/// The records are laid out on one thread that the set keeps until it is finished or
/// dropped. A record that fails ends the set: every later call returns
/// [`Error::RecordSet`], and what the writer has is not a whole PDF file.
pub struct RecordSet<W: Write> {
    writer: W,
    worker: Worker,
    limits: Limits,
    pages: usize,
    records: usize,
    /// Why the set cannot go on, once a record has failed.
    failed: Option<String>,
}

impl<W: Write> RecordSet<W> {
    /// Starts a set that writes its PDF file to `writer`. Nothing is written before the
    /// first record.
    pub fn open(writer: W, options: &RenderOptions) -> Result<Self, Error> {
        check_scale(options)?;
        Ok(Self {
            writer,
            worker: Worker::start(options.clone())?,
            limits: options.limits.clone(),
            pages: 0,
            records: 0,
            failed: None,
        })
    }

    /// Prints `html` as the next record, on the pages after those written so far. When
    /// this returns, the writer has the record's pages and nothing of the record is kept.
    pub fn add(&mut self, html: &str) -> Result<Added, Error> {
        self.usable()?;
        let written = check_input_size(html, &self.limits)
            .and_then(|()| self.worker.ask(Work::Add(html.to_owned())))
            .and_then(|written| self.write(written));
        match written {
            Ok(written) => {
                self.pages += written.pages;
                self.records += 1;
                Ok(Added {
                    pages: written.pages,
                    warnings: written.warnings,
                })
            }
            Err(error) => {
                self.failed = Some(error.to_string());
                self.worker.stop();
                Err(error)
            }
        }
    }

    /// How many pages are written so far.
    pub fn pages(&self) -> usize {
        self.pages
    }

    /// The writer. Between two calls it has every byte of the file so far, so a caller
    /// that keeps the file elsewhere can take what the writer holds after every record.
    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Writes what the whole file shares, such as its fonts and its page tree, flushes
    /// the writer and returns it. A set without records is a file without pages.
    pub fn finish(mut self) -> Result<(W, Summary), Error> {
        self.usable()?;
        let written = self.worker.ask(Work::Finish)?;
        self.write(written)?;
        self.writer.flush().map_err(write_error)?;
        let summary = Summary {
            pages: self.pages,
            records: self.records,
        };
        Ok((self.writer, summary))
    }

    fn usable(&self) -> Result<(), Error> {
        match &self.failed {
            Some(first) => Err(Error::RecordSet(format!(
                "an earlier record failed: {first}"
            ))),
            None => Ok(()),
        }
    }

    fn write(&mut self, written: Written) -> Result<Written, Error> {
        self.writer.write_all(&written.bytes).map_err(write_error)?;
        Ok(Written {
            bytes: Vec::new(),
            ..written
        })
    }
}

fn write_error(error: std::io::Error) -> Error {
    Error::Write(error.to_string())
}

/// What the set asks of its thread.
enum Work {
    /// Print a record.
    Add(String),
    /// End the file.
    Finish,
}

/// What the thread wrote for one piece of [`Work`].
struct Written {
    pages: usize,
    warnings: Vec<String>,
    /// The bytes of the file that follow those handed over before.
    bytes: Vec<u8>,
}

/// The thread that lays the records out and writes them. Fonts are cached per thread and
/// told apart by where they are in memory, so one thread prints every record of a set:
/// that is what embeds a font once for the whole file.
struct Worker {
    /// `None` once the thread is told to stop.
    work: Option<Sender<Work>>,
    done: Receiver<Result<Written, Error>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    fn start(options: RenderOptions) -> Result<Self, Error> {
        let (work, jobs) = mpsc::channel();
        let (results, done) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("rustyprint-set".into())
            .stack_size(STACK_SIZE)
            .spawn(move || Printer::new(options).run(&jobs, &results))
            .map_err(|error| {
                Error::Internal(format!("could not start a render thread: {error}"))
            })?;
        Ok(Self {
            work: Some(work),
            done,
            thread: Some(thread),
        })
    }

    /// Has the thread do `work`, and waits for it.
    fn ask(&self, work: Work) -> Result<Written, Error> {
        let stopped = || Error::Internal("the thread of the record set has stopped".into());
        let sender = self.work.as_ref().ok_or_else(stopped)?;
        sender.send(work).map_err(|_| stopped())?;
        self.done.recv().map_err(|_| stopped())?
    }

    /// Ends the thread, which is waiting for work whenever the set is not inside a call.
    fn stop(&mut self) {
        self.work = None;
        if let Some(thread) = self.thread.take() {
            // The thread catches the panics of its work, so there is nothing to pass on.
            let _ = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What the thread of a set holds between records: the options, the images that records
/// share, and the PDF file being written, which holds the fonts and what tells the
/// images written apart, and none of the pages.
struct Printer {
    options: RenderOptions,
    images: ImageCache,
    /// The file, from the first record on. It is written to memory, a record at a time,
    /// and the set hands each record's bytes to its writer.
    session: Option<Session<Vec<u8>>>,
    /// The color profiles the file was started with: those of the first record.
    color_profiles: Vec<ColorProfile>,
    device_cmyk: Option<u32>,
}

impl Printer {
    fn new(options: RenderOptions) -> Self {
        Self {
            options,
            images: ImageCache::new(SHARED_IMAGE_BYTES),
            session: None,
            color_profiles: Vec::new(),
            device_cmyk: None,
        }
    }

    /// Does the work the set asks for until the set is dropped, the file is finished or
    /// a record fails: a budget that is exceeded leaves this thread's caches as they were
    /// in the middle of the record, so the thread does not print another.
    fn run(mut self, jobs: &Receiver<Work>, results: &Sender<Result<Written, Error>>) {
        for work in jobs {
            let (result, last) = match work {
                Work::Add(html) => {
                    let result = self.within_limits(|printer| printer.add(&html));
                    let failed = result.is_err();
                    (result, failed)
                }
                Work::Finish => (self.within_limits(Self::finish), true),
            };
            if results.send(result).is_err() || last {
                return;
            }
        }
    }

    /// Runs `work` within the limits of one record, with a panic as an error.
    fn within_limits(
        &mut self,
        work: impl FnOnce(&mut Self) -> Result<Written, Error>,
    ) -> Result<Written, Error> {
        let limits = self.options.limits.clone();
        let within = || budget::run(budget_of(&limits), || work(self));
        match panic::catch_unwind(AssertUnwindSafe(within)) {
            Err(panic) => Err(Error::Internal(panic_message(&*panic))),
            Ok(Err(over)) => Err(limit_error(over, &limits)),
            Ok(Ok(result)) => result,
        }
    }

    fn add(&mut self, html: &str) -> Result<Written, Error> {
        self.images.next_document();
        let before = self.session.as_ref().map_or(0, Session::page_count);
        let preceding = Preceding {
            pages: before,
            images: Some(&self.images),
        };
        let (record, warnings) = lay_out_after(html, &self.options, &preceding)?;
        let pages = record.pages.len();
        let max = self.options.limits.max_total_pages;
        if before + pages > max {
            return Err(exceeded(Limit::TotalPages, max));
        }
        let session = self.session_for(&record)?;
        session.add_pages(&record).map_err(internal)?;
        Ok(Written {
            pages,
            warnings,
            bytes: std::mem::take(session.get_mut()),
        })
    }

    /// The file, started with the title and color profiles of `record` if it is the
    /// first. The colors of a page refer to the file's profiles by index, so a later
    /// record has to have the same ones.
    fn session_for(&mut self, record: &Document) -> Result<&mut Session<Vec<u8>>, Error> {
        match &mut self.session {
            Some(session) => {
                if record.color_profiles != self.color_profiles
                    || record.device_cmyk != self.device_cmyk
                {
                    return Err(Error::RecordSet(
                        "the color profiles of this record are not those of the first".into(),
                    ));
                }
                Ok(session)
            }
            empty => {
                let session = Session::new(Vec::new(), Setup::of(record)).map_err(internal)?;
                self.color_profiles.clone_from(&record.color_profiles);
                self.device_cmyk = record.device_cmyk;
                Ok(empty.insert(session))
            }
        }
    }

    fn finish(&mut self) -> Result<Written, Error> {
        let session = match self.session.take() {
            Some(session) => session,
            None => {
                let setup = Setup {
                    title: self.options.title.as_deref(),
                    ..Setup::default()
                };
                Session::new(Vec::new(), setup).map_err(internal)?
            }
        };
        Ok(Written {
            pages: 0,
            warnings: Vec::new(),
            bytes: session.finish().map_err(internal)?,
        })
    }
}

fn internal(error: rustyprint_pdf::Error) -> Error {
    Error::Internal(error.to_string())
}
