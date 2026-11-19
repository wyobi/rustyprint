//! Record sets: many documents printed one after another as one PDF file, with one of them
//! in memory at a time.

use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use rustyprint_css::stylesheet::Stylesheet;
use rustyprint_dom::{NodeData, NodeId, budget, local_name};
use rustyprint_layout::display::{ColorProfile, Document};
use rustyprint_layout::{Carried, DeferredMargins, ImageCache, Images, LaterTotal};
use rustyprint_pdf::{Session, Setup};

use crate::{
    Error, Fit, LaidFlow, Limit, Limits, Preceding, RenderOptions, STACK_SIZE, assets, budget_of,
    check_input_size, check_scale, exceeded, lay_out_flow, limit_error, panic_message, parse,
};

/// The attribute of a `<table>` at the start of a piece that continues: its rows are
/// more rows of the table that the piece before ended with.
const CONTINUES_TABLE: &str = "data-rp-continue";

/// The decoded pixels a set keeps, at the most, of the images that its records share.
const SHARED_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// A record added to a [`RecordSet`].
#[derive(Debug, Clone, PartialEq)]
pub struct Added {
    /// How many pages the record added to the set: the pages it took, or for one that
    /// continues on the page of the record before it, the pages it started.
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
/// - `counter(pages)` in a page's margin is the number of pages of the whole set. It is
///   known when the set is finished, so a margin box that shows it is painted then, on a
///   page that is already written; until then the set keeps the style, number and size of
///   every such page, and nothing of what is on it. A box beside it on the same edge
///   waits with it when its place depends on how wide the total is; if that box shows a
///   running element or an image, the set keeps that as well. Relative URLs in the style
///   of a box that waits are loaded without the record's `<base>`.
///   `target-counter(url, pages)` in a record's content counts the record's own pages,
///   with a warning.
/// - Links and `target-counter()` reach inside a record only: a link to an element of
///   another record does not resolve.
/// - The file's title is [`RenderOptions::title`], or else the `<title>` of the first
///   record. Its color profiles are those of the first record, and a record with other
///   `@color-profile` rules fails.
/// - Fonts are loaded once and embedded once: the bundled ones, and those of `@font-face`
///   rules, whose files are told apart by their bytes, so the font of a rule in every
///   record is one font of the file. The set keeps up to 64 MB of font files, with the
///   ones used the longest ago let go first; a font used again after that is embedded
///   again. The stylesheet of the options is parsed once. An image is written to the file once
///   however many records use it. Decoded images are kept between records only once a
///   second record uses the same file, up to 64 MB of pixels with the ones used the
///   longest ago let go first, so a logo is decoded twice and a record's own photo is
///   freed with the record.
///
/// # Records that continue
///
/// [`RecordSet::add_continuing`] adds a piece of the document that the record before it
/// started, instead of a document of its own: its content is laid out as if it were at
/// the end of that record's `<body>`, on the page where that record ended. So a long
/// table can be printed a few hundred rows at a time, and comes out as it does printed
/// whole. A record and the pieces that continue it are a *flow*.
///
/// - The last page of a flow is open: it is a page of the file, and what it paints is
///   written when it is full, when a record that does not continue is added, or when the
///   set is finished. Between two calls the set holds what that page paints, and the part
///   of the flow's document that is on it or after it.
/// - A piece that starts with `<table data-rp-continue>` adds the rows of that table's
///   `<tbody>` elements to the last `<tbody>` of the table that the flow ends with, which
///   has to be the last thing in the flow so far and a child of `<body>`. The `<colgroup>`,
///   `<thead>`, `<tfoot>` and `<caption>` of the piece's table are not used: the table keeps
///   those of its first piece, and the header and footer repeat on every page as they do
///   in a table printed whole. Without the attribute a table is a table of its own. With
///   it and no table to continue, it is a table of its own too, with a warning.
/// - A table that is a child of `<body>` keeps the column widths it had when it was first
///   laid out. For `table-layout: fixed`, and for an automatic table whose widths its first
///   rows or its `<col>` elements decide, those are the widths of the table printed whole;
///   wider content in later rows wraps in the columns as they are. Rows with more columns
///   than the table started with fail the `add`.
/// - Collapsed borders, `:nth-child()`, `:nth-of-type()`, `:first-child`, sibling
///   combinators, and counters that the rows add to with `counter-increment` carry on over
///   the pieces. `:nth-child(… of S)` counts the rows that are let go of as matching `S`,
///   and `:nth-last-child()` and `:last-child` see the rows there are so far.
/// - The styles of a flow are those of its first piece: a `<style>` or stylesheet link in
///   the `<head>` of a piece that continues is not used, with a warning unless the first
///   piece had the same one. So is its `<base>`, and its `<title>`.
/// - An element is kept for as long as the flow goes on, rather than let go of when its
///   page is written, when it or something in it has `position: fixed` or
///   `position: running()`, `counter-reset` or `counter-set`, `display: list-item`, or is a
///   `<style>` or `<link>`. A `position: fixed` box is repeated on the pages laid out with
///   the piece that has it and with the pieces up to the one that fills its page, not on
///   the pages after those.
/// - Links and `target-counter()` reach what the flow still holds: the open page and what
///   follows it.
/// - With [`RenderOptions::shrink_to_fit`], a flow is shrunk as its first piece needs.
/// - The first record of a set starts a flow whichever way it is added.
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

    /// Prints `html` as the next record, on the pages after those of the records so far.
    /// When this returns, the writer has the record's pages. What its last page paints is
    /// written with the record after it, or when the set is finished: the page is open
    /// for a record that continues, see [`RecordSet::add_continuing`].
    pub fn add(&mut self, html: &str) -> Result<Added, Error> {
        self.add_as(html, false)
    }

    /// Prints `html` as more of the record before it: its content follows that record's
    /// on the same page, as if it were at the end of that record's `<body>`. See
    /// *Records that continue* on [`RecordSet`]. When this returns, the writer has the
    /// pages that are full.
    pub fn add_continuing(&mut self, html: &str) -> Result<Added, Error> {
        self.add_as(html, true)
    }

    fn add_as(&mut self, html: &str, continues: bool) -> Result<Added, Error> {
        self.usable()?;
        let written = check_input_size(html, &self.limits)
            .and_then(|()| self.worker.ask(Work::Add(html.to_owned(), continues)))
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

    /// How many pages the set has so far.
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
    /// Print a record, which continues the one before it or does not.
    Add(String, bool),
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
/// told apart by where they are in memory, so one thread prints every record of a set and
/// keeps the font files the records load: that is what embeds a font once for the whole
/// file.
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

/// What the thread of a set holds between records: the options, the images and the font
/// files that records share, and the PDF file being written, which holds the fonts and what tells the
/// images written apart, and none of the pages.
struct Printer {
    options: RenderOptions,
    /// The stylesheet of the options, parsed once for every record.
    stylesheet: Option<Stylesheet>,
    images: ImageCache,
    /// The font files the records loaded, each of them once.
    fonts: assets::FontFiles,
    /// The file, from the first record on. It is written to memory, a record at a time,
    /// and the set hands each record's bytes to its writer.
    session: Option<Session<Vec<u8>>>,
    /// The color profiles the file was started with: those of the first record.
    color_profiles: Vec<ColorProfile>,
    device_cmyk: Option<u32>,
    total: LaterTotal,
    /// The margin boxes that wait for the number of pages, by page of the file from zero.
    deferred: Vec<(usize, DeferredMargins)>,
    /// The flow that the last record is part of, whose last page is not written.
    flow: Option<Flow>,
}

/// A record and the pieces that continue it, between two of them.
struct Flow {
    /// The flow's document, from the content of its open page on.
    document: rustyprint_dom::Document,
    carried: Carried,
    fit: Fit,
    /// The margin boxes of the open page that wait for the number of pages.
    deferred: Option<DeferredMargins>,
}

impl Printer {
    fn new(options: RenderOptions) -> Self {
        Self {
            stylesheet: options.stylesheet.as_deref().map(Stylesheet::parse),
            options,
            images: ImageCache::new(SHARED_IMAGE_BYTES),
            fonts: assets::FontFiles::default(),
            session: None,
            color_profiles: Vec::new(),
            device_cmyk: None,
            total: LaterTotal::new(),
            deferred: Vec::new(),
            flow: None,
        }
    }

    /// Does the work the set asks for until the set is dropped, the file is finished or
    /// a record fails: a budget that is exceeded leaves this thread's caches as they were
    /// in the middle of the record, so the thread does not print another.
    fn run(mut self, jobs: &Receiver<Work>, results: &Sender<Result<Written, Error>>) {
        for work in jobs {
            let (result, last) = match work {
                Work::Add(html, continues) => {
                    let result = self.within_limits(|printer| printer.add(&html, continues));
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

    fn add(&mut self, html: &str, continues: bool) -> Result<Written, Error> {
        self.images.next_document();
        let piece = parse(html);
        let mut warnings = Vec::new();
        // A piece that continues is added to the document of its flow, which is laid out
        // again from its open page. Any other record ends the flow before it.
        let (mut document, carried) = match self.flow.take() {
            Some(flow) if continues => {
                let mut document = flow.document;
                let table = flow.carried.last_table(&document);
                warnings.extend(append(&mut document, &piece, table));
                budget::check_document(&document);
                (document, Some((flow.carried, flow.fit)))
            }
            Some(flow) => {
                self.end(flow);
                (piece, None)
            }
            None => (piece, None),
        };
        // The open page of a flow that carries on is laid out again: it is the first page
        // of this record.
        let written = self.session.as_ref().map_or(0, Session::page_count);
        let before = written - usize::from(carried.is_some());
        let preceding = Preceding {
            pages: before,
            images: Some(&self.images),
            total: Some(&self.total),
            fonts: Some(&self.fonts),
            stylesheet: self.stylesheet.as_ref(),
        };
        let kept = carried.as_ref().map(|(carried, fit)| (carried, *fit));
        let LaidFlow {
            laid_out: (record, laid_warnings, deferred),
            open,
            fit,
        } = lay_out_flow(&document, &self.options, &preceding, kept)?;
        warnings.extend(laid_warnings);
        let pages = record.pages.len();
        let max = self.options.limits.max_total_pages;
        if before + pages > max {
            return Err(exceeded(Limit::TotalPages, max));
        }
        let last = pages.saturating_sub(1);
        let later: Vec<usize> = deferred.iter().map(|(page, _)| *page).collect();
        let continued = carried.is_some();
        let session = self.session_for(&record)?;
        if !continued {
            session.close_open_page().map_err(internal)?;
        }
        session
            .add_pages_keeping_open(&record, &later)
            .map_err(internal)?;
        let bytes = std::mem::take(session.get_mut());
        let mut open_deferred = None;
        for (page, margins) in deferred {
            if page == last {
                open_deferred = Some(margins);
            } else {
                self.deferred.push((before + page, margins));
            }
        }
        let carried = rustyprint_layout::let_go(&mut document, open);
        self.flow = Some(Flow {
            document,
            carried,
            fit,
            deferred: open_deferred,
        });
        Ok(Written {
            // The open page of a flow is counted with the record that started the flow.
            pages: if continued { last } else { pages },
            warnings,
            bytes,
        })
    }

    /// Ends a flow: the margin boxes of its open page are those the page has for good.
    fn end(&mut self, flow: Flow) {
        let page = self.session.as_ref().map_or(0, Session::page_count);
        let margins = flow
            .deferred
            .map(|margins| (page.saturating_sub(1), margins));
        self.deferred.extend(margins);
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
        if let Some(flow) = self.flow.take() {
            self.end(flow);
        }
        let mut session = match self.session.take() {
            Some(session) => session,
            None => {
                let setup = Setup {
                    title: self.options.title.as_deref(),
                    ..Setup::default()
                };
                Session::new(Vec::new(), setup).map_err(internal)?
            }
        };
        session.close_open_page().map_err(internal)?;
        self.paint_deferred(&mut session)?;
        Ok(Written {
            pages: 0,
            warnings: Vec::new(),
            bytes: session.finish().map_err(internal)?,
        })
    }
}

impl Printer {
    /// Paints the margin boxes that waited for the number of pages, now that the file
    /// has all of them, each on its page.
    fn paint_deferred(&mut self, session: &mut Session<Vec<u8>>) -> Result<(), Error> {
        let total = session.page_count();
        let loader = assets::Loader::new(&self.options.assets);
        let load_file = |url: &str| loader.get(url);
        let images = Images::new(&load_file)
            .with_max_pixels(self.options.limits.max_image_pixels)
            .with_max_svg_pixels(self.options.limits.max_svg_pixels)
            .with_cache(&self.images);
        for (page, margins) in std::mem::take(&mut self.deferred) {
            budget::check_deadline();
            let items = rustyprint_layout::paint_deferred_margins(&margins, total, &images);
            session.paint_later(page, &items).map_err(internal)?;
        }
        Ok(())
    }
}

fn internal(error: rustyprint_pdf::Error) -> Error {
    Error::Internal(error.to_string())
}

/// The `<body>` of a document.
fn body_of(document: &rustyprint_dom::Document) -> Option<NodeId> {
    let html = document.document_element()?;
    document.element_children(html).find(|&child| {
        document
            .element(child)
            .is_some_and(|e| e.is_html(&local_name!("body")))
    })
}

/// The stylesheets in `within`, an element of a document, or in the whole document: the
/// text of each `<style>` and the address of each `<link>`.
fn stylesheets(document: &rustyprint_dom::Document, within: Option<NodeId>) -> Vec<String> {
    let nodes = document.descendants(within.unwrap_or(document.root()));
    nodes
        .filter_map(|node| {
            let element = document.element(node)?;
            if element.is_html(&local_name!("style")) {
                Some(document.text_content(node))
            } else if element.is_html(&local_name!("link")) {
                element.attr("href").map(str::to_owned)
            } else {
                None
            }
        })
        .collect()
}

/// Adds the content of `piece` to the end of `document`, the document of the flow that
/// the piece continues, and returns the warnings of doing so. `table` is the table the
/// flow ends with, if it ends with one: a `<table data-rp-continue>` that the piece starts
/// with gives its rows to that table.
fn append(
    document: &mut rustyprint_dom::Document,
    piece: &rustyprint_dom::Document,
    table: Option<NodeId>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let (Some(body), Some(from)) = (body_of(document), body_of(piece)) else {
        warnings.push("a record that continues was not added: a document has no <body>".into());
        return warnings;
    };
    let known = stylesheets(document, None);
    let head = piece.find_html(&local_name!("head"));
    let unused = head.map_or(Vec::new(), |head| stylesheets(piece, Some(head)));
    if unused.iter().any(|style| !known.contains(style)) {
        warnings.push(
            "a stylesheet in the <head> of a record that continues is not used: the styles \
             are those of the record it continues"
                .into(),
        );
    }
    let mut started = false;
    for child in piece.children(from) {
        let blank = match piece.node(child).data() {
            NodeData::Text(text) => text.trim().is_empty(),
            NodeData::Element(_) => false,
            _ => true,
        };
        let first = !started && !blank;
        started |= !blank;
        let continues = first
            && piece
                .element(child)
                .is_some_and(|e| e.is_html(&local_name!("table")) && e.has_attr(CONTINUES_TABLE));
        match (continues, table) {
            (true, Some(table)) => add_rows(document, table, piece, child),
            (true, None) => {
                warnings.push(format!(
                    "{CONTINUES_TABLE}: the record before does not end with a table that is a \
                     child of <body>, so this table is one of its own"
                ));
                document.adopt(body, piece, child);
            }
            (false, _) => {
                document.adopt(body, piece, child);
            }
        }
    }
    warnings
}

/// Adds the rows of the `<tbody>` elements of `from`, a table of `piece`, to the last
/// `<tbody>` of `table`, or its `<tbody>` elements themselves if `table` has none.
fn add_rows(
    document: &mut rustyprint_dom::Document,
    table: NodeId,
    piece: &rustyprint_dom::Document,
    from: NodeId,
) {
    let is_body = |document: &rustyprint_dom::Document, node: NodeId| {
        let element = document.element(node);
        element.is_some_and(|e| e.is_html(&local_name!("tbody")))
    };
    let last_body = document
        .element_children(table)
        .filter(|&child| is_body(document, child))
        .last();
    let groups = piece
        .element_children(from)
        .filter(|&child| is_body(piece, child));
    for group in groups {
        match last_body {
            Some(body) => {
                for row in piece.element_children(group) {
                    document.adopt(body, piece, row);
                }
            }
            None => {
                document.adopt(table, piece, group);
            }
        }
    }
}
