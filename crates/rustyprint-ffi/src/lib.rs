//! A C ABI over [`rustyprint::render`], built as the shared library that C programs, the Ruby
//! gem and the .NET package load. The header, `include/rustyprint.h`, is generated from this
//! file by cbindgen.
//!
//! A program makes a request from JSON options, adds the files the document refers to, renders
//! HTML with it and reads the PDF, the warnings or the error from the result:
//!
//! ```c
//! rp_request *request = rp_request_new(options, strlen(options));
//! rp_request_add_asset(request, "img/logo.png", logo, logo_len);
//! rp_result *result = NULL;
//! if (rp_render(request, html, strlen(html), &result) == RP_STATUS_OK) {
//!     size_t len;
//!     const uint8_t *pdf = rp_result_pdf(result, &len);
//!     fwrite(pdf, 1, len, file);
//! } else {
//!     fprintf(stderr, "%s\n", rp_result_error(result, NULL));
//! }
//! rp_result_free(result);
//! rp_request_free(request);
//! ```
//!
//! A record set prints many documents as one PDF file, one record at a time, and hands over
//! the bytes of the file as they are written, so neither side holds the whole file:
//!
//! ```c
//! rp_record_set *set = NULL;
//! size_t len;
//! const uint8_t *bytes;
//! if (rp_record_set_open(request, &set) == RP_STATUS_OK) {
//!     while (next_record(&html) &&
//!            rp_record_set_add(set, html, strlen(html)) == RP_STATUS_OK) {
//!         bytes = rp_record_set_take_output(set, &len);
//!         fwrite(bytes, 1, len, file);
//!     }
//!     if (rp_record_set_finish(set) == RP_STATUS_OK) {
//!         bytes = rp_record_set_take_output(set, &len);
//!         fwrite(bytes, 1, len, file);
//!     }
//! }
//! if (rp_record_set_error(set, NULL)) fprintf(stderr, "%s\n", rp_record_set_error(set, NULL));
//! rp_record_set_free(set);
//! ```
//!
//! - Options cross as JSON, in the format of `schema/render-options.schema.json`, so new options
//!   never change the ABI.
//! - Text is UTF-8. Options and HTML are a pointer and a length, and need not end in NUL;
//!   asset paths end in NUL. Strings the library gives end in NUL.
//! - Requests, results and record sets are opaque, and only the library frees them, with
//!   [`rp_request_free`], [`rp_result_free`] and [`rp_record_set_free`].
//! - No entry point unwinds into the caller: a panic is caught and reported as
//!   `RP_STATUS_INTERNAL`.
//! - A request may render on several threads at once, as long as no thread adds assets to it
//!   meanwhile. Each render runs on a thread of its own with a large stack, so the calling
//!   thread's stack size does not matter.
//! - A record set is used by one thread at a time, and may move from one thread to another
//!   between calls. It lays its records out on a thread of its own, which it keeps until it
//!   is finished, a record fails or it is freed.

use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::{ptr, slice};

use rustyprint::{Error, Output, RenderOptions, Size};

/// What a call did. A render's status is also its result's.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The call succeeded.
    Ok = 0,
    /// An argument is null where it may not be, or is not UTF-8.
    InvalidArgument = 1,
    /// The JSON options are not valid.
    InvalidOptions = 2,
    /// The document exceeds one of the limits in the options.
    LimitExceeded = 3,
    /// Rendering took longer than the deadline in the options.
    Timeout = 4,
    /// A bug in rustyprint, such as a caught panic.
    Internal = 5,
    /// The record set cannot go on: it did not open, a record failed earlier or it is
    /// finished.
    RecordSet = 6,
}

/// Options and assets for rendering documents. Made by `rp_request_new` and freed by
/// `rp_request_free`.
pub struct Request {
    /// The options with the assets added so far, or why the JSON options are invalid.
    options: Result<RenderOptions, String>,
}

/// What a render gave: a PDF and its warnings, or an error. Made by `rp_render`, or by
/// `rp_content_width` with a width in place of the PDF, and freed by `rp_result_free`.
pub struct RenderResult {
    status: Status,
    pdf: Vec<u8>,
    /// The width `rp_content_width` measured, in CSS pixels.
    content_width: f64,
    /// A JSON array of strings.
    warnings: CString,
    error: Option<CString>,
}

impl RenderResult {
    fn output(output: Output) -> Self {
        let warnings = serde_json::to_string(&output.warnings).unwrap_or_else(|_| "[]".into());
        Self {
            status: Status::Ok,
            pdf: output.pdf,
            content_width: 0.0,
            warnings: c_string(warnings),
            error: None,
        }
    }

    fn content_width(width: f32) -> Self {
        Self {
            status: Status::Ok,
            pdf: Vec::new(),
            content_width: f64::from(width),
            warnings: c"[]".to_owned(),
            error: None,
        }
    }

    fn error(status: Status, message: impl Into<String>) -> Self {
        Self {
            status,
            pdf: Vec::new(),
            content_width: 0.0,
            warnings: c"[]".to_owned(),
            error: Some(c_string(message.into())),
        }
    }
}

/// A PDF file printed one record at a time, which hands over the bytes of the file as they
/// are written. Made by `rp_record_set_open` and freed by `rp_record_set_free`.
pub struct RecordSet {
    /// The set while it takes records. Its writer holds the bytes written and not taken yet.
    set: Option<rustyprint::RecordSet<Vec<u8>>>,
    /// Why there is no set: it did not open, it is finished, or it did not finish.
    ended: String,
    /// The bytes not taken yet of a set that is finished.
    rest: Vec<u8>,
    /// The bytes `rp_record_set_take_output` handed over last.
    taken: Vec<u8>,
    /// The pages of the record added last.
    added_pages: usize,
    /// The warnings of the record added last, as a JSON array of strings.
    warnings: CString,
    /// Why the last call failed.
    error: Option<CString>,
    pages: usize,
    records: usize,
}

impl RecordSet {
    fn new(set: Result<rustyprint::RecordSet<Vec<u8>>, String>) -> Self {
        let (set, ended, error) = match set {
            Ok(set) => (Some(set), String::new(), None),
            Err(message) => (
                None,
                ended(&format!("it did not open: {message}")),
                Some(c_string(message)),
            ),
        };
        Self {
            set,
            ended,
            rest: Vec::new(),
            taken: Vec::new(),
            added_pages: 0,
            warnings: c"[]".to_owned(),
            error,
            pages: 0,
            records: 0,
        }
    }

    /// Forgets what the last call gave: the bytes taken, the record's pages and warnings, and
    /// the error.
    fn next_call(&mut self) {
        self.taken = Vec::new();
        self.added_pages = 0;
        self.warnings = c"[]".to_owned();
        self.error = None;
    }

    fn fail(&mut self, status: Status, message: impl Into<String>) -> Status {
        self.error = Some(c_string(message.into()));
        status
    }

    fn add(&mut self, html: &str, continues: bool) -> Status {
        let Some(set) = &mut self.set else {
            return self.fail(Status::RecordSet, self.ended.clone());
        };
        let added = if continues {
            set.add_continuing(html)
        } else {
            set.add(html)
        };
        match added {
            Ok(added) => {
                self.pages = set.pages();
                self.records += 1;
                self.added_pages = added.pages;
                self.warnings = c_string(
                    serde_json::to_string(&added.warnings).unwrap_or_else(|_| "[]".into()),
                );
                Status::Ok
            }
            Err(error) => self.fail(status(&error), error.to_string()),
        }
    }

    fn finish(&mut self) -> Status {
        let Some(set) = self.set.take() else {
            return self.fail(Status::RecordSet, self.ended.clone());
        };
        match set.finish() {
            Ok((bytes, summary)) => {
                self.rest = bytes;
                self.pages = summary.pages;
                self.records = summary.records;
                self.ended = ended("it is finished");
                Status::Ok
            }
            Err(error) => {
                self.ended = ended(&format!("it did not finish: {error}"));
                self.fail(status(&error), error.to_string())
            }
        }
    }
}

/// Why a set that has ended takes no more calls, worded as the library's own errors are.
fn ended(why: &str) -> String {
    Error::RecordSet(why.to_owned()).to_string()
}

/// `text` as a C string, with any NUL in it replaced.
fn c_string(text: String) -> CString {
    CString::new(text.replace('\0', "\u{fffd}")).unwrap_or_default()
}

fn status(error: &Error) -> Status {
    match error {
        Error::InvalidOptions(_) => Status::InvalidOptions,
        Error::LimitExceeded { .. } => Status::LimitExceeded,
        Error::Timeout => Status::Timeout,
        Error::RecordSet(_) => Status::RecordSet,
        _ => Status::Internal,
    }
}

/// Runs `f`, or `on_panic` if it panics, so that no panic unwinds into the caller.
fn guard<T>(on_panic: impl FnOnce() -> T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| on_panic())
}

/// The `len` bytes at `data`, or `None` if `data` is null and `len` is not zero.
///
/// # Safety
///
/// Unless `len` is zero, `data` is null or points to `len` bytes that stay unchanged for `'a`.
unsafe fn bytes<'a>(data: *const u8, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        Some(&[])
    } else if data.is_null() {
        None
    } else {
        // SAFETY: the caller guarantees `len` readable bytes at `data`.
        Some(unsafe { slice::from_raw_parts(data, len) })
    }
}

/// Stores `value` in `*len`, unless `len` is null.
///
/// # Safety
///
/// `len` is null or writable.
unsafe fn set_len(len: *mut usize, value: usize) {
    if !len.is_null() {
        // SAFETY: the caller guarantees `len` is writable.
        unsafe { *len = value };
    }
}

/// The library's version, such as `1.2.0`, as a static string.
#[unsafe(no_mangle)]
pub extern "C" fn rp_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// Makes a request from JSON options: `len` bytes of UTF-8 at `options_json`, in the format of
/// `schema/render-options.schema.json`. A length of zero gives the default options.
///
/// Invalid options still give a request; rendering with it fails with
/// `RP_STATUS_INVALID_OPTIONS` and says why. Returns null only if the library fails.
///
/// # Safety
///
/// Unless `len` is zero, `options_json` is null or points to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_request_new(options_json: *const c_char, len: usize) -> *mut Request {
    guard(ptr::null_mut, || {
        // SAFETY: the caller guarantees the bytes are readable.
        let options = match unsafe { bytes(options_json.cast(), len) } {
            None => Err(format!(
                "invalid options: options_json is null but len is {len}"
            )),
            Some([]) => Ok(RenderOptions::default()),
            Some(json) => match std::str::from_utf8(json) {
                Ok(json) => RenderOptions::from_json(json).map_err(|e| e.to_string()),
                Err(e) => Err(format!("invalid options: the JSON is not UTF-8: {e}")),
            },
        };
        Box::into_raw(Box::new(Request { options }))
    })
}

/// Adds a file the document can refer to, such as an image, stylesheet or font: `len` bytes at
/// `data`, under `path`, the NUL-terminated UTF-8 path the document uses, such as
/// `img/logo.png`. The data is copied. A path added again replaces the earlier file.
///
/// Returns `RP_STATUS_INVALID_ARGUMENT` if `request` or `path` is null, `path` is not UTF-8,
/// or `data` is null with a length that is not zero.
///
/// # Safety
///
/// `request` is null or a request no other thread is using. `path` is null or a NUL-terminated
/// string. Unless `len` is zero, `data` is null or points to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_request_add_asset(
    request: *mut Request,
    path: *const c_char,
    data: *const u8,
    len: usize,
) -> Status {
    guard(
        || Status::Internal,
        || {
            // SAFETY: the caller guarantees the request is live and not in use elsewhere.
            let Some(request) = (unsafe { request.as_mut() }) else {
                return Status::InvalidArgument;
            };
            if path.is_null() {
                return Status::InvalidArgument;
            }
            // SAFETY: the caller guarantees the path is NUL-terminated.
            let Ok(path) = unsafe { std::ffi::CStr::from_ptr(path) }.to_str() else {
                return Status::InvalidArgument;
            };
            // SAFETY: the caller guarantees the bytes are readable.
            let Some(data) = (unsafe { bytes(data, len) }) else {
                return Status::InvalidArgument;
            };
            // Assets of a request with invalid options are dropped: it cannot render.
            if let Ok(options) = &mut request.options {
                options.assets.files.insert(path.to_owned(), data.to_vec());
            }
            Status::Ok
        },
    )
}

/// Renders `len` bytes of UTF-8 HTML at `html` with the request's options and assets, and
/// stores the result in `*out`, which the caller frees with `rp_result_free`. Returns the
/// result's status: `RP_STATUS_OK` with a PDF, or an error with a message.
///
/// Every call with a non-null `out` stores a result there, even when it fails, unless the
/// library cannot allocate one. A null `out` returns `RP_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `request` is null or a live request that no thread is adding assets to. Unless `len` is
/// zero, `html` is null or points to `len` readable bytes. `out` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_render(
    request: *const Request,
    html: *const c_char,
    len: usize,
    out: *mut *mut RenderResult,
) -> Status {
    if out.is_null() {
        return Status::InvalidArgument;
    }
    let result = guard(
        || RenderResult::error(Status::Internal, "internal error: rp_render panicked"),
        // SAFETY: the caller's guarantees are those `render` needs.
        || unsafe { render(request, html, len) },
    );
    let status = result.status;
    // SAFETY: `out` is not null, and the caller guarantees it is writable.
    unsafe { *out = Box::into_raw(Box::new(result)) };
    status
}

/// # Safety
///
/// As for [`rp_render`].
unsafe fn render(request: *const Request, html: *const c_char, len: usize) -> RenderResult {
    // SAFETY: the caller's guarantees are those `with_document` needs.
    unsafe {
        with_document(request, html, len, |html, options| {
            rustyprint::render(html, options).map(RenderResult::output)
        })
    }
}

/// Measures the width, in CSS pixels, of the content of `len` bytes of UTF-8 HTML at `html` in
/// a browser window `window_width` by `window_height` CSS pixels in size: the window's width,
/// or more when content reaches past its right edge. The document is laid out for the screen,
/// on one continuous page. Of the request's options, the stylesheet, the limits and the
/// assets apply.
///
/// Stores the result in `*out`, which the caller frees with `rp_result_free`, and returns its
/// status: `RP_STATUS_OK` with a width that `rp_result_content_width` gives, or an error with
/// a message. A null `out` returns `RP_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// As for [`rp_render`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_content_width(
    request: *const Request,
    html: *const c_char,
    len: usize,
    window_width: f64,
    window_height: f64,
    out: *mut *mut RenderResult,
) -> Status {
    if out.is_null() {
        return Status::InvalidArgument;
    }
    let window = Size::new(window_width as f32, window_height as f32);
    let result = guard(
        || {
            RenderResult::error(
                Status::Internal,
                "internal error: rp_content_width panicked",
            )
        },
        // SAFETY: the caller's guarantees are those `with_document` needs.
        || unsafe {
            with_document(request, html, len, |html, options| {
                rustyprint::content_width(html, window, options).map(RenderResult::content_width)
            })
        },
    );
    let status = result.status;
    // SAFETY: `out` is not null, and the caller guarantees it is writable.
    unsafe { *out = Box::into_raw(Box::new(result)) };
    status
}

/// The result of `work` on the document and the request's options, or of what is wrong with
/// either.
///
/// # Safety
///
/// As for [`rp_render`].
unsafe fn with_document(
    request: *const Request,
    html: *const c_char,
    len: usize,
    work: impl FnOnce(&str, &RenderOptions) -> Result<RenderResult, Error>,
) -> RenderResult {
    // SAFETY: the caller guarantees the request is null or live.
    let Some(request) = (unsafe { request.as_ref() }) else {
        return RenderResult::error(Status::InvalidArgument, "request is null");
    };
    // SAFETY: the caller guarantees the bytes are readable.
    let Some(html) = (unsafe { bytes(html.cast(), len) }) else {
        return RenderResult::error(
            Status::InvalidArgument,
            format!("html is null but len is {len}"),
        );
    };
    let html = match std::str::from_utf8(html) {
        Ok(html) => html,
        Err(e) => {
            return RenderResult::error(Status::InvalidArgument, format!("html is not UTF-8: {e}"));
        }
    };
    let options = match &request.options {
        Ok(options) => options,
        Err(message) => return RenderResult::error(Status::InvalidOptions, message.clone()),
    };
    work(html, options)
        .unwrap_or_else(|error| RenderResult::error(status(&error), error.to_string()))
}

/// The width `rp_content_width` measured, in CSS pixels. Zero if it failed, for a result of
/// `rp_render` and for a null result.
///
/// # Safety
///
/// `result` is null or a live result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_result_content_width(result: *const RenderResult) -> f64 {
    // SAFETY: the caller guarantees the result is null or live.
    guard(
        || 0.0,
        || unsafe { result.as_ref() }.map_or(0.0, |r| r.content_width),
    )
}

/// The result's status, the one `rp_render` returned. `RP_STATUS_INVALID_ARGUMENT` for a null
/// result.
///
/// # Safety
///
/// `result` is null or a live result.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_result_status(result: *const RenderResult) -> Status {
    // SAFETY: the caller guarantees the result is null or live.
    guard(
        || Status::Internal,
        || unsafe { result.as_ref() }.map_or(Status::InvalidArgument, |r| r.status),
    )
}

/// The PDF, with its length in bytes stored in `*len` unless `len` is null. Null, with a
/// length of zero, if the render failed. The bytes live as long as the result.
///
/// # Safety
///
/// `result` is null or a live result. `len` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_result_pdf(result: *const RenderResult, len: *mut usize) -> *const u8 {
    guard(ptr::null, || {
        // SAFETY: the caller guarantees the result is null or live.
        let pdf = unsafe { result.as_ref() }
            .filter(|r| r.status == Status::Ok)
            .map(|r| &r.pdf);
        // SAFETY: the caller guarantees `len` is null or writable.
        unsafe { set_len(len, pdf.map_or(0, Vec::len)) };
        pdf.map_or(ptr::null(), |pdf| pdf.as_ptr())
    })
}

/// The render's warnings, as a NUL-terminated JSON array of strings such as
/// `` ["style.css:3: unsupported property `colr`"] ``, with its length in bytes, not counting the NUL,
/// stored in `*len` unless `len` is null. `[]` if there are none or the render failed; null
/// only for a null result. The string lives as long as the result.
///
/// # Safety
///
/// `result` is null or a live result. `len` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_result_warnings_json(
    result: *const RenderResult,
    len: *mut usize,
) -> *const c_char {
    // SAFETY: the caller guarantees the result is null or live.
    guard(ptr::null, || unsafe {
        string(result.as_ref().map(|r| r.warnings.as_c_str()), len)
    })
}

/// Why the render failed, as a NUL-terminated message, with its length in bytes, not counting
/// the NUL, stored in `*len` unless `len` is null. Null, with a length of zero, if the render
/// succeeded. The string lives as long as the result.
///
/// # Safety
///
/// `result` is null or a live result. `len` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_result_error(
    result: *const RenderResult,
    len: *mut usize,
) -> *const c_char {
    // SAFETY: the caller guarantees the result is null or live.
    guard(ptr::null, || unsafe {
        string(result.as_ref().and_then(|r| r.error.as_deref()), len)
    })
}

/// # Safety
///
/// `len` is null or writable.
unsafe fn string(text: Option<&std::ffi::CStr>, len: *mut usize) -> *const c_char {
    // SAFETY: the caller guarantees `len` is null or writable.
    unsafe { set_len(len, text.map_or(0, |t| t.count_bytes())) };
    text.map_or(ptr::null(), std::ffi::CStr::as_ptr)
}

/// Opens a record set, which prints many documents as one PDF file one record at a time, with
/// the request's options and assets, which are copied: the request may be freed or used for
/// other renders afterwards. Every record is a document of its own with those options; page
/// numbers run through the set, and `counter(pages)` is the number of pages of the whole set.
/// Stores the set in `*out`, which the caller frees with `rp_record_set_free`, and returns
/// `RP_STATUS_OK`, or why the set did not open, with a message that `rp_record_set_error`
/// gives.
///
/// Every call with a non-null `out` stores a set there, even when it fails, unless the
/// library cannot allocate one. A null `out` returns `RP_STATUS_INVALID_ARGUMENT`.
///
/// The set lays its records out on a thread of its own, which ends when the set is finished,
/// when a record fails and when the set is freed.
///
/// # Safety
///
/// `request` is null or a live request that no thread is adding assets to. `out` is null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_open(
    request: *const Request,
    out: *mut *mut RecordSet,
) -> Status {
    if out.is_null() {
        return Status::InvalidArgument;
    }
    let unopened = |status, message: String| (status, RecordSet::new(Err(message)));
    let (status, set) = guard(
        || {
            unopened(
                Status::Internal,
                "internal error: rp_record_set_open panicked".into(),
            )
        },
        || {
            // SAFETY: the caller guarantees the request is null or live.
            let Some(request) = (unsafe { request.as_ref() }) else {
                return unopened(Status::InvalidArgument, "request is null".into());
            };
            let options = match &request.options {
                Ok(options) => options,
                Err(message) => return unopened(Status::InvalidOptions, message.clone()),
            };
            match rustyprint::RecordSet::open(Vec::new(), options) {
                Ok(set) => (Status::Ok, RecordSet::new(Ok(set))),
                Err(error) => unopened(status(&error), error.to_string()),
            }
        },
    );
    // SAFETY: `out` is not null, and the caller guarantees it is writable.
    unsafe { *out = Box::into_raw(Box::new(set)) };
    status
}

/// Prints `len` bytes of UTF-8 HTML at `html` as the next record of the set, on the pages
/// after those of the records so far. Returns `RP_STATUS_OK`, with the bytes of the pages
/// that are written ready for `rp_record_set_take_output`, the record's pages for
/// `rp_record_set_added_pages` and its warnings for `rp_record_set_warnings_json`; or why it
/// failed, with a message that `rp_record_set_error` gives. The last page of the record is
/// open for a record that continues it (`rp_record_set_add_continuing`): what it paints is
/// written with the next record or when the set is finished. Besides that page and the
/// bytes, nothing of the record is kept, so a caller that takes the bytes after every
/// record has the library hold one record at a time.
///
/// A record that fails ends the set: what was written is not a whole PDF file, and every
/// later call fails with `RP_STATUS_RECORD_SET`. So does a call on a set that did not open
/// or is finished. HTML that is null or not UTF-8 is `RP_STATUS_INVALID_ARGUMENT`, and
/// leaves the set as it was. A null set returns `RP_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `set` is null or a live record set no other thread is using. Unless `len` is zero, `html`
/// is null or points to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_add(
    set: *mut RecordSet,
    html: *const c_char,
    len: usize,
) -> Status {
    guard(
        || Status::Internal,
        || {
            // SAFETY: the caller guarantees the set is null, or live and not in use elsewhere.
            let Some(set) = (unsafe { set.as_mut() }) else {
                return Status::InvalidArgument;
            };
            set.next_call();
            // SAFETY: the caller guarantees the bytes are readable.
            let Some(html) = (unsafe { bytes(html.cast(), len) }) else {
                return set.fail(
                    Status::InvalidArgument,
                    format!("html is null but len is {len}"),
                );
            };
            match std::str::from_utf8(html) {
                Ok(html) => set.add(html, false),
                Err(e) => set.fail(Status::InvalidArgument, format!("html is not UTF-8: {e}")),
            }
        },
    )
}

/// Prints `len` bytes of UTF-8 HTML at `html` as more of the record before it: its content
/// follows that record's on the same page, as if it were at the end of that record's
/// `<body>`, so a document can be printed a piece at a time and come out as it does printed
/// whole. A piece that starts with `<table data-rp-continue>` adds the rows of that table to
/// the table the record before ended with, which keeps its columns, header and footer.
/// `rp_record_set_added_pages` is then the pages the piece started, which is none when it
/// fits on the open page. Everything else is as `rp_record_set_add` has it; the first record
/// of a set can be added either way.
///
/// # Safety
///
/// `set` is null or a live record set no other thread is using. Unless `len` is zero, `html`
/// is null or points to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_add_continuing(
    set: *mut RecordSet,
    html: *const c_char,
    len: usize,
) -> Status {
    guard(
        || Status::Internal,
        || {
            // SAFETY: the caller guarantees the set is null, or live and not in use elsewhere.
            let Some(set) = (unsafe { set.as_mut() }) else {
                return Status::InvalidArgument;
            };
            set.next_call();
            // SAFETY: the caller guarantees the bytes are readable.
            let Some(html) = (unsafe { bytes(html.cast(), len) }) else {
                return set.fail(
                    Status::InvalidArgument,
                    format!("html is null but len is {len}"),
                );
            };
            match std::str::from_utf8(html) {
                Ok(html) => set.add(html, true),
                Err(e) => set.fail(Status::InvalidArgument, format!("html is not UTF-8: {e}")),
            }
        },
    )
}

/// Ends the file: writes what the whole file shares, such as its fonts, its page tree and
/// the number of pages that `counter(pages)` prints. Returns `RP_STATUS_OK`, with those last
/// bytes ready for `rp_record_set_take_output`; or why it failed, with a message that
/// `rp_record_set_error` gives. A set without records is a file without pages.
///
/// The set takes no more records, and still has to be freed. A null set returns
/// `RP_STATUS_INVALID_ARGUMENT`.
///
/// # Safety
///
/// `set` is null or a live record set no other thread is using.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_finish(set: *mut RecordSet) -> Status {
    guard(
        || Status::Internal,
        || {
            // SAFETY: the caller guarantees the set is null, or live and not in use elsewhere.
            let Some(set) = (unsafe { set.as_mut() }) else {
                return Status::InvalidArgument;
            };
            set.next_call();
            set.finish()
        },
    )
}

/// Takes the bytes of the file written since the last take: those that follow the bytes
/// taken before. Their length is stored in `*len` unless `len` is null. Null, with a length
/// of zero, when nothing was written since, and for a null set.
///
/// The bytes live until the next `rp_record_set_add`, `rp_record_set_finish` or
/// `rp_record_set_take_output` with the set, or until it is freed; the set holds them
/// nowhere else. A caller takes them after every record and after finishing, in that order,
/// and has the whole file; bytes not taken wait in the set and come with the next take.
///
/// # Safety
///
/// `set` is null or a live record set no other thread is using. `len` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_take_output(
    set: *mut RecordSet,
    len: *mut usize,
) -> *const u8 {
    guard(ptr::null, || {
        // SAFETY: the caller guarantees the set is null, or live and not in use elsewhere.
        let taken = unsafe { set.as_mut() }.map(|set| {
            set.taken = match &mut set.set {
                Some(records) => std::mem::take(records.writer_mut()),
                None => std::mem::take(&mut set.rest),
            };
            &set.taken
        });
        // SAFETY: the caller guarantees `len` is null or writable.
        unsafe { set_len(len, taken.map_or(0, Vec::len)) };
        taken
            .filter(|bytes| !bytes.is_empty())
            .map_or(ptr::null(), |bytes| bytes.as_ptr())
    })
}

/// How many pages the record added last took. Zero if it failed, after
/// `rp_record_set_finish` and for a null set.
///
/// # Safety
///
/// `set` is null or a live record set no other thread is using.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_added_pages(set: *const RecordSet) -> usize {
    // SAFETY: the caller guarantees the set is null or live.
    guard(
        || 0,
        || unsafe { set.as_ref() }.map_or(0, |s| s.added_pages),
    )
}

/// The warnings of the record added last, as a NUL-terminated JSON array of strings, with
/// its length in bytes, not counting the NUL, stored in `*len` unless `len` is null. `[]` if
/// there are none, if the record failed and after `rp_record_set_finish`; null only for a
/// null set. The string lives until the next `rp_record_set_add` or `rp_record_set_finish`
/// with the set, or until it is freed.
///
/// # Safety
///
/// `set` is null or a live record set no other thread is using. `len` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_warnings_json(
    set: *const RecordSet,
    len: *mut usize,
) -> *const c_char {
    // SAFETY: the caller guarantees the set is null or live.
    guard(ptr::null, || unsafe {
        string(set.as_ref().map(|s| s.warnings.as_c_str()), len)
    })
}

/// Why the last `rp_record_set_open`, `rp_record_set_add` or `rp_record_set_finish` with the
/// set failed, as a NUL-terminated message, with its length in bytes, not counting the NUL,
/// stored in `*len` unless `len` is null. Null, with a length of zero, if it succeeded. The
/// string lives until the next `rp_record_set_add` or `rp_record_set_finish` with the set,
/// or until it is freed.
///
/// # Safety
///
/// `set` is null or a live record set no other thread is using. `len` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_error(
    set: *const RecordSet,
    len: *mut usize,
) -> *const c_char {
    // SAFETY: the caller guarantees the set is null or live.
    guard(ptr::null, || unsafe {
        string(set.as_ref().and_then(|s| s.error.as_deref()), len)
    })
}

/// How many pages the set has so far: after `rp_record_set_finish`, the pages of the file.
/// Zero for a null set.
///
/// # Safety
///
/// `set` is null or a live record set no other thread is using.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_pages(set: *const RecordSet) -> usize {
    // SAFETY: the caller guarantees the set is null or live.
    guard(|| 0, || unsafe { set.as_ref() }.map_or(0, |s| s.pages))
}

/// How many records the set has so far: after `rp_record_set_finish`, the records of the
/// file. Zero for a null set.
///
/// # Safety
///
/// `set` is null or a live record set no other thread is using.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_records(set: *const RecordSet) -> usize {
    // SAFETY: the caller guarantees the set is null or live.
    guard(|| 0, || unsafe { set.as_ref() }.map_or(0, |s| s.records))
}

/// Frees a record set, with the bytes, warnings and error it holds, and ends its thread. A
/// set may be freed at any point: one that is not finished is given up, and what was written
/// is not a whole PDF file. Null does nothing.
///
/// # Safety
///
/// `set` is null or a live record set from `rp_record_set_open`, which no other thread is
/// using and which is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_record_set_free(set: *mut RecordSet) {
    if !set.is_null() {
        // SAFETY: the caller guarantees the set came from `rp_record_set_open` and is not used
        // again.
        guard(|| (), || drop(unsafe { Box::from_raw(set) }));
    }
}

/// Frees a request. Null does nothing.
///
/// # Safety
///
/// `request` is null or a live request from `rp_request_new`, which no other thread is using
/// and which is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_request_free(request: *mut Request) {
    if !request.is_null() {
        // SAFETY: the caller guarantees the request came from `rp_request_new` and is not used
        // again.
        guard(|| (), || drop(unsafe { Box::from_raw(request) }));
    }
}

/// Frees a result, and with it the PDF, warnings and error it holds. Null does nothing.
///
/// # Safety
///
/// `result` is null or a live result from `rp_render` or `rp_content_width`, which is not used
/// again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rp_result_free(result: *mut RenderResult) {
    if !result.is_null() {
        // SAFETY: the caller guarantees the result came from `rp_render` and is not used again.
        guard(|| (), || drop(unsafe { Box::from_raw(result) }));
    }
}
