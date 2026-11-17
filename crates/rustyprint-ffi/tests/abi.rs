//! The C ABI, called from Rust as a C program would call it.

use std::ffi::{CStr, c_char};
use std::ptr;
use std::thread;

use rustyprint_ffi::{
    RecordSet, RenderResult as RpResult, Request, Status, rp_content_width, rp_record_set_add,
    rp_record_set_add_continuing, rp_record_set_added_pages, rp_record_set_error,
    rp_record_set_finish, rp_record_set_free, rp_record_set_open, rp_record_set_pages,
    rp_record_set_records, rp_record_set_take_output, rp_record_set_warnings_json, rp_render,
    rp_request_add_asset, rp_request_free, rp_request_new, rp_result_content_width,
    rp_result_error, rp_result_free, rp_result_pdf, rp_result_status, rp_result_warnings_json,
    rp_version,
};

/// A request with the given JSON options, or the defaults for `None`.
fn request(options: Option<&str>) -> *mut Request {
    let request = match options {
        // SAFETY: the pointer and length describe a live string.
        Some(json) => unsafe { rp_request_new(json.as_ptr().cast(), json.len()) },
        // SAFETY: no options is a null pointer and a length of zero.
        None => unsafe { rp_request_new(ptr::null(), 0) },
    };
    assert!(!request.is_null());
    request
}

fn add_asset(request: *mut Request, path: &CStr, data: &[u8]) -> Status {
    // SAFETY: the request is live, the path is NUL-terminated and the data is a live slice.
    unsafe { rp_request_add_asset(request, path.as_ptr(), data.as_ptr(), data.len()) }
}

/// A finished render: its status and what the result holds, read through the ABI.
struct Rendered {
    status: Status,
    result_status: Status,
    pdf: Option<Vec<u8>>,
    warnings: Vec<String>,
    error: Option<String>,
}

fn render_bytes(request: *const Request, html: &[u8]) -> Rendered {
    let mut result: *mut RpResult = ptr::null_mut();
    // SAFETY: the request is live or null, the HTML is a live slice and `result` is writable.
    let status = unsafe { rp_render(request, html.as_ptr().cast(), html.len(), &mut result) };
    assert!(!result.is_null(), "every render gives a result");
    // SAFETY: the result is live until it is freed below.
    let rendered = unsafe { read(result) };
    assert_eq!(rendered.result_status, status);
    // SAFETY: the result came from `rp_render` and is freed once.
    unsafe { rp_result_free(result) };
    Rendered { status, ..rendered }
}

fn render(request: *const Request, html: &str) -> Rendered {
    render_bytes(request, html.as_bytes())
}

/// # Safety
///
/// `result` must be a live result.
unsafe fn read(result: *const RpResult) -> Rendered {
    let mut len = usize::MAX;
    // SAFETY: the caller gives a live result, and `len` is writable.
    let pdf = unsafe { rp_result_pdf(result, &mut len) };
    let pdf = if pdf.is_null() {
        assert_eq!(len, 0, "no PDF has a length of zero");
        None
    } else {
        // SAFETY: the library gives `len` bytes at `pdf`, live as long as the result.
        Some(unsafe { std::slice::from_raw_parts(pdf, len) }.to_vec())
    };
    // SAFETY: as above.
    let warnings = unsafe { rp_result_warnings_json(result, &mut len) };
    assert!(!warnings.is_null(), "warnings are always given");
    // SAFETY: the warnings are a NUL-terminated string `len` bytes long.
    let warnings = unsafe { CStr::from_ptr(warnings) }.to_str().unwrap();
    assert_eq!(warnings.len(), len);
    let warnings = serde_json::from_str(warnings).unwrap();
    // SAFETY: as above.
    let error = unsafe { rp_result_error(result, &mut len) };
    let error = if error.is_null() {
        assert_eq!(len, 0);
        None
    } else {
        // SAFETY: the error is a NUL-terminated string `len` bytes long.
        let error = unsafe { CStr::from_ptr(error) }
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(error.len(), len);
        Some(error)
    };
    Rendered {
        // SAFETY: as above.
        status: unsafe { rp_result_status(result) },
        // SAFETY: as above.
        result_status: unsafe { rp_result_status(result) },
        pdf,
        warnings,
        error,
    }
}

fn free(request: *mut Request) {
    // SAFETY: the request came from `rp_request_new` and is freed once.
    unsafe { rp_request_free(request) };
}

fn pages(pdf: &[u8]) -> Vec<[f32; 4]> {
    let document = lopdf::Document::load_mem(pdf).unwrap();
    document
        .page_iter()
        .map(|id| {
            let page = document.get_dictionary(id).unwrap();
            let media_box = page.get(b"MediaBox").unwrap().as_array().unwrap();
            let number = |i: usize| media_box[i].as_float().unwrap();
            [number(0), number(1), number(2), number(3)]
        })
        .collect()
}

#[test]
fn renders_html_to_pdf() {
    let request = request(Some(r#"{"page_size": "A5", "title": "Hello"}"#));
    let rendered = render(request, "<h1>Hello</h1>");
    free(request);
    assert_eq!(rendered.status, Status::Ok);
    assert_eq!(rendered.error, None);
    assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    let pdf = rendered.pdf.unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    // A5 is 148 by 210 mm.
    let [_, _, width, height] = pages(&pdf)[0];
    assert!((width - 419.5).abs() < 1.0, "{width}");
    assert!((height - 595.3).abs() < 1.0, "{height}");
}

#[test]
fn no_options_are_the_defaults() {
    let defaults = request(None);
    let empty = request(Some("{}"));
    let from_null = render(defaults, "<p>Hello</p>");
    let from_empty = render(empty, "<p>Hello</p>");
    free(defaults);
    free(empty);
    assert_eq!(from_null.status, Status::Ok);
    // A4 is 210 by 297 mm.
    let [_, _, width, _] = pages(&from_null.pdf.unwrap())[0];
    assert!((width - 595.3).abs() < 1.0, "{width}");
    assert_eq!(from_empty.status, Status::Ok);
}

#[test]
fn assets_are_used_by_the_path_the_document_gives() {
    let request = request(None);
    let css = b"@page { size: 300px 200px }";
    assert_eq!(add_asset(request, c"css/page.css", css), Status::Ok);
    let rendered = render(
        request,
        r#"<link rel="stylesheet" href="css/page.css"><p>Hello</p>"#,
    );
    free(request);
    assert_eq!(rendered.status, Status::Ok);
    // 300 by 200 CSS pixels are 225 by 150 points.
    assert_eq!(pages(&rendered.pdf.unwrap())[0], [0.0, 0.0, 225.0, 150.0]);
}

#[test]
fn an_asset_added_twice_keeps_the_last() {
    let request = request(None);
    add_asset(request, c"page.css", b"@page { size: 100px 100px }");
    add_asset(request, c"page.css", b"@page { size: 400px 400px }");
    let rendered = render(request, r#"<link rel="stylesheet" href="page.css">"#);
    free(request);
    assert_eq!(pages(&rendered.pdf.unwrap())[0], [0.0, 0.0, 300.0, 300.0]);
}

#[test]
fn warnings_are_a_json_array() {
    let request = request(None);
    let rendered = render(
        request,
        "<style>p { colör: red; colr: blue }</style><p>x</p>",
    );
    free(request);
    assert_eq!(rendered.status, Status::Ok);
    assert_eq!(rendered.warnings.len(), 2, "{:?}", rendered.warnings);
    assert!(rendered.warnings.iter().any(|w| w.contains("colör")));
}

#[test]
fn invalid_options_fail_the_render() {
    for options in [r#"{"page_size": "B99"}"#, r#"{"colour": "red"}"#, "{", "[]"] {
        let request = request(Some(options));
        let rendered = render(request, "<p>Hello</p>");
        free(request);
        assert_eq!(rendered.status, Status::InvalidOptions, "{options}");
        assert_eq!(rendered.pdf, None);
        assert!(rendered.warnings.is_empty());
        assert!(
            rendered.error.unwrap().contains("invalid options"),
            "{options}"
        );
    }
}

#[test]
fn options_that_are_not_utf8_are_invalid() {
    let json = b"{\"title\": \"\xff\"}";
    // SAFETY: the pointer and length describe a live slice.
    let request = unsafe { rp_request_new(json.as_ptr().cast(), json.len()) };
    let rendered = render(request, "<p>Hello</p>");
    free(request);
    assert_eq!(rendered.status, Status::InvalidOptions);
    assert!(rendered.error.unwrap().contains("UTF-8"));
}

#[test]
fn a_limit_exceeded_fails_the_render() {
    let request = request(Some(r#"{"limits": {"max_input_bytes": 10}}"#));
    let rendered = render(request, "<p>More than ten bytes</p>");
    free(request);
    assert_eq!(rendered.status, Status::LimitExceeded);
    assert_eq!(rendered.pdf, None);
    assert!(rendered.error.unwrap().contains("input size"));
}

#[test]
fn the_deadline_fails_the_render() {
    let request = request(Some(r#"{"limits": {"timeout_ms": 1}}"#));
    let html = "<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit.</p>".repeat(5_000);
    let rendered = render(request, &html);
    free(request);
    assert_eq!(rendered.status, Status::Timeout);
    assert!(rendered.error.unwrap().contains("deadline"));
}

#[test]
fn a_panic_is_an_internal_error() {
    let request = request(None);
    let rendered = render(request, "<!--rustyprint:panic--><p>Hello</p>");
    free(request);
    assert_eq!(rendered.status, Status::Internal);
    assert_eq!(rendered.pdf, None);
    assert!(rendered.error.unwrap().contains("internal error"));
}

#[test]
fn html_that_is_not_utf8_is_an_invalid_argument() {
    let request = request(None);
    let rendered = render_bytes(request, b"<p>\xff</p>");
    free(request);
    assert_eq!(rendered.status, Status::InvalidArgument);
    assert!(rendered.error.unwrap().contains("UTF-8"));
}

#[test]
fn null_arguments_are_invalid() {
    let rendered = render(ptr::null(), "<p>Hello</p>");
    assert_eq!(rendered.status, Status::InvalidArgument);
    assert!(rendered.error.unwrap().contains("request"));

    let request = request(None);
    let rendered = render_bytes(request, &[]);
    assert_eq!(
        rendered.status,
        Status::Ok,
        "an empty document is a document"
    );
    let mut result: *mut RpResult = ptr::null_mut();
    // SAFETY: a null HTML pointer with a length is what is being tested; nothing is read.
    let status = unsafe { rp_render(request, ptr::null(), 5, &mut result) };
    assert_eq!(status, Status::InvalidArgument);
    // SAFETY: the result is live, then freed once.
    unsafe {
        assert!(read(result).error.unwrap().contains("html"));
        rp_result_free(result);
    }
    // SAFETY: a null result pointer is what is being tested.
    let status = unsafe { rp_render(request, c"<p>".as_ptr(), 3, ptr::null_mut()) };
    assert_eq!(status, Status::InvalidArgument);

    // SAFETY: null arguments are what is being tested; nothing is read through them.
    unsafe {
        assert_eq!(
            rp_request_add_asset(ptr::null_mut(), c"a.css".as_ptr(), ptr::null(), 0),
            Status::InvalidArgument
        );
        assert_eq!(
            rp_request_add_asset(request, ptr::null(), ptr::null(), 0),
            Status::InvalidArgument
        );
        assert_eq!(
            rp_request_add_asset(request, c"a.css".as_ptr(), ptr::null(), 3),
            Status::InvalidArgument
        );
        assert_eq!(
            rp_request_add_asset(request, c"empty.css".as_ptr(), ptr::null(), 0),
            Status::Ok,
            "an empty asset is an asset"
        );
        let bad_path: &[u8] = b"\xff.css\0";
        assert_eq!(
            rp_request_add_asset(request, bad_path.as_ptr().cast::<c_char>(), ptr::null(), 0),
            Status::InvalidArgument
        );
        // A null options pointer with a length is not valid either.
        let invalid = rp_request_new(ptr::null(), 4);
        let rendered = render(invalid, "<p>Hello</p>");
        assert_eq!(rendered.status, Status::InvalidOptions);
        rp_request_free(invalid);
    }
    free(request);
}

/// The status, width and error `rp_content_width` gives for a window 800 by 600.
fn content_width(request: *const Request, html: &str) -> (Status, f64, Option<String>) {
    let mut result: *mut RpResult = ptr::null_mut();
    // SAFETY: the request is live or null, the HTML is a live string and `result` is writable.
    let status = unsafe {
        rp_content_width(
            request,
            html.as_ptr().cast(),
            html.len(),
            800.0,
            600.0,
            &mut result,
        )
    };
    assert!(!result.is_null(), "every measurement gives a result");
    // SAFETY: the result is live until it is freed below.
    let (width, error) = unsafe {
        assert_eq!(rp_result_status(result), status);
        let mut len = usize::MAX;
        rp_result_pdf(result, &mut len);
        assert_eq!(len, 0, "a measurement has no PDF");
        let error = rp_result_error(result, ptr::null_mut());
        let error = (!error.is_null()).then(|| CStr::from_ptr(error).to_str().unwrap().to_owned());
        (rp_result_content_width(result), error)
    };
    // SAFETY: the result came from `rp_content_width` and is freed once.
    unsafe { rp_result_free(result) };
    (status, width, error)
}

#[test]
fn content_width_is_the_width_of_the_widest_content() {
    let request = request(None);
    let fits = content_width(request, "<p>Hello</p>");
    assert_eq!(fits, (Status::Ok, 800.0, None));
    let wide = content_width(
        request,
        "<body style='margin: 0'><div style='width: 1000px'>Wide</div>",
    );
    assert_eq!(wide, (Status::Ok, 1000.0, None));
    free(request);
}

#[test]
fn content_width_uses_the_request_assets() {
    let request = request(None);
    let status = add_asset(request, c"wide.css", b"div { width: 1200px }");
    assert_eq!(status, Status::Ok);
    let html = "<link rel='stylesheet' href='wide.css'><body style='margin: 0'><div>Wide</div>";
    assert_eq!(content_width(request, html), (Status::Ok, 1200.0, None));
    free(request);
}

#[test]
fn content_width_fails_as_a_render_does() {
    let invalid = request(Some(r#"{"page_size": "A9"}"#));
    let (status, width, error) = content_width(invalid, "<p>Hello</p>");
    assert_eq!((status, width), (Status::InvalidOptions, 0.0));
    assert!(error.unwrap().contains("A9"));
    free(invalid);

    let (status, _, error) = content_width(ptr::null(), "<p>Hello</p>");
    assert_eq!(status, Status::InvalidArgument);
    assert_eq!(error.as_deref(), Some("request is null"));

    let request = request(None);
    // SAFETY: a null `out` is refused before anything is read.
    let status =
        unsafe { rp_content_width(request, c"<p>".as_ptr(), 3, 800.0, 600.0, ptr::null_mut()) };
    assert_eq!(status, Status::InvalidArgument);
    // SAFETY: a null result is allowed.
    assert_eq!(unsafe { rp_result_content_width(ptr::null()) }, 0.0);
    let rendered = render(request, "<p>Hello</p>");
    assert_eq!(rendered.status, Status::Ok);
    free(request);
}

#[test]
fn freeing_null_does_nothing() {
    // SAFETY: freeing null is allowed.
    unsafe {
        rp_request_free(ptr::null_mut());
        rp_result_free(ptr::null_mut());
    }
}

#[test]
fn a_request_renders_on_several_threads_at_once() {
    let request = request(Some(r#"{"page_size": "A5"}"#)) as usize;
    let lengths: Vec<usize> = thread::scope(|scope| {
        let threads: Vec<_> = (0..4)
            .map(|i| {
                scope.spawn(move || {
                    let rendered = render(request as *const Request, &format!("<p>{i}</p>"));
                    assert_eq!(rendered.status, Status::Ok);
                    rendered.pdf.unwrap().len()
                })
            })
            .collect();
        threads.into_iter().map(|t| t.join().unwrap()).collect()
    });
    free(request as *mut Request);
    assert!(lengths.iter().all(|&len| len > 0));
}

/// A record set opened with the given JSON options, and the status of opening it.
fn open_set(options: Option<&str>) -> (Status, *mut RecordSet) {
    let request = request(options);
    let mut set: *mut RecordSet = ptr::null_mut();
    // SAFETY: the request is live and `set` is writable.
    let status = unsafe { rp_record_set_open(request, &mut set) };
    assert!(!set.is_null(), "every open gives a set");
    // The set has its own copy of the options.
    free(request);
    (status, set)
}

fn add_record(set: *mut RecordSet, html: &str) -> Status {
    // SAFETY: the set is live and the HTML is a live string.
    unsafe { rp_record_set_add(set, html.as_ptr().cast(), html.len()) }
}

/// The bytes the set wrote since the last take.
fn take_output(set: *mut RecordSet) -> Vec<u8> {
    let mut len = usize::MAX;
    // SAFETY: the set is live and `len` is writable.
    let bytes = unsafe { rp_record_set_take_output(set, &mut len) };
    if bytes.is_null() {
        assert_eq!(len, 0, "no bytes have a length of zero");
        return Vec::new();
    }
    // SAFETY: the library gives `len` bytes at `bytes`, live until the next call.
    unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec()
}

fn set_error(set: *const RecordSet) -> Option<String> {
    // SAFETY: the set is live.
    let error = unsafe { rp_record_set_error(set, ptr::null_mut()) };
    (!error.is_null()).then(|| {
        // SAFETY: the error is a NUL-terminated string.
        unsafe { CStr::from_ptr(error) }
            .to_str()
            .unwrap()
            .to_owned()
    })
}

fn free_set(set: *mut RecordSet) {
    // SAFETY: the set came from `rp_record_set_open` and is freed once.
    unsafe { rp_record_set_free(set) };
}

#[test]
fn a_record_set_hands_over_one_pdf_a_record_at_a_time() {
    let (status, set) = open_set(Some(r#"{"page_size": "A5"}"#));
    assert_eq!(status, Status::Ok);
    assert_eq!(set_error(set), None);

    let records = [
        "<style>p { colr: red }</style><p>One</p>",
        "<p>Two</p><p style='break-before: page'>Two, again</p>",
        "<p>Three</p>",
    ];
    let mut pdf = Vec::new();
    for (record, pages) in records.into_iter().zip([1, 2, 1]) {
        assert_eq!(add_record(set, record), Status::Ok);
        // SAFETY: the set is live.
        assert_eq!(unsafe { rp_record_set_added_pages(set) }, pages);
        let taken = take_output(set);
        assert!(!taken.is_empty(), "a record's pages are written by its add");
        pdf.extend(taken);
        assert!(take_output(set).is_empty(), "the bytes are taken once");
    }
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(lopdf::Document::load_mem(&pdf).is_err(), "not a file yet");

    // SAFETY: the set is live.
    assert_eq!(unsafe { rp_record_set_finish(set) }, Status::Ok);
    pdf.extend(take_output(set));
    // SAFETY: the set is live.
    let summary = unsafe { (rp_record_set_pages(set), rp_record_set_records(set)) };
    assert_eq!(summary, (4, 3));
    assert_eq!(pages(&pdf).len(), 4);

    assert_eq!(add_record(set, "<p>Four</p>"), Status::RecordSet);
    assert!(set_error(set).unwrap().contains("finished"));
    free_set(set);
}

#[test]
fn a_record_that_continues_goes_on_the_page_of_the_record_before_it() {
    let (status, set) = open_set(Some(r#"{"page_size": "A5"}"#));
    assert_eq!(status, Status::Ok);
    let row = |n: usize| format!("<tr><td>Row {n}</td></tr>");
    let pieces = [
        format!("<table><tbody>{}</tbody></table>", row(1)),
        format!("<table data-rp-continue><tbody>{}</tbody></table>", row(2)),
        "<p>After the table</p>".to_owned(),
    ];
    let mut pdf = Vec::new();
    for (index, (piece, pages)) in pieces.iter().zip([1, 0, 0]).enumerate() {
        let status = if index == 0 {
            add_record(set, piece)
        } else {
            // SAFETY: the set is live and the HTML is a live string.
            unsafe { rp_record_set_add_continuing(set, piece.as_ptr().cast(), piece.len()) }
        };
        assert_eq!(status, Status::Ok, "{:?}", set_error(set));
        // SAFETY: the set is live.
        assert_eq!(unsafe { rp_record_set_added_pages(set) }, pages);
        pdf.extend(take_output(set));
    }
    // SAFETY: the set is live.
    assert_eq!(unsafe { rp_record_set_finish(set) }, Status::Ok);
    pdf.extend(take_output(set));
    // SAFETY: the set is live.
    let summary = unsafe { (rp_record_set_pages(set), rp_record_set_records(set)) };
    assert_eq!(summary, (1, 3));
    let document = lopdf::Document::load_mem(&pdf).unwrap();
    assert_eq!(document.get_pages().len(), 1);
    let text = document.extract_text(&[1]).unwrap();
    for shown in ["Row 1", "Row 2", "After the table"] {
        assert!(text.contains(shown), "{text}");
    }

    // A null set and HTML that is not there are refused as `rp_record_set_add` refuses them.
    // SAFETY: a null set is allowed, and so is null HTML with a length.
    unsafe {
        assert_eq!(
            rp_record_set_add_continuing(ptr::null_mut(), c"x".as_ptr(), 1),
            Status::InvalidArgument
        );
        assert_eq!(
            rp_record_set_add_continuing(set, ptr::null(), 5),
            Status::InvalidArgument
        );
    }
    free_set(set);
}

#[test]
fn a_record_set_gives_the_warnings_of_the_last_record() {
    let (_, set) = open_set(None);
    let warnings = |set: *const RecordSet| {
        let mut len = usize::MAX;
        // SAFETY: the set is live and `len` is writable.
        let warnings = unsafe { rp_record_set_warnings_json(set, &mut len) };
        // SAFETY: the warnings are a NUL-terminated string `len` bytes long.
        let warnings = unsafe { CStr::from_ptr(warnings) }.to_str().unwrap();
        assert_eq!(warnings.len(), len);
        serde_json::from_str::<Vec<String>>(warnings).unwrap()
    };
    assert_eq!(
        add_record(set, "<style>p { colr: red }</style><p>x</p>"),
        Status::Ok
    );
    assert!(warnings(set).join("").contains("colr"));
    assert_eq!(add_record(set, "<p>x</p>"), Status::Ok);
    assert!(warnings(set).is_empty());
    free_set(set);
}

#[test]
fn bytes_not_taken_come_with_the_next_take() {
    let (_, set) = open_set(None);
    assert_eq!(add_record(set, "<p>One</p>"), Status::Ok);
    assert_eq!(add_record(set, "<p>Two</p>"), Status::Ok);
    // SAFETY: the set is live.
    assert_eq!(unsafe { rp_record_set_finish(set) }, Status::Ok);
    assert_eq!(pages(&take_output(set)).len(), 2);
    free_set(set);
}

#[test]
fn a_record_that_fails_ends_the_set() {
    let (status, set) = open_set(Some(r#"{"limits": {"max_total_pages": 2}}"#));
    assert_eq!(status, Status::Ok);
    assert_eq!(add_record(set, "<p>One</p>"), Status::Ok);
    let two_pages = "<p>Two</p><p style='break-before: page'>Three</p>";
    assert_eq!(add_record(set, two_pages), Status::LimitExceeded);
    assert!(set_error(set).is_some());
    // SAFETY: the set is live.
    assert_eq!(unsafe { rp_record_set_added_pages(set) }, 0);

    assert_eq!(add_record(set, "<p>Four</p>"), Status::RecordSet);
    assert!(set_error(set).unwrap().contains("an earlier record failed"));
    // SAFETY: the set is live.
    assert_eq!(unsafe { rp_record_set_finish(set) }, Status::RecordSet);
    // SAFETY: the set is live.
    assert_eq!(unsafe { rp_record_set_records(set) }, 1);
    free_set(set);
}

#[test]
fn a_record_set_with_invalid_options_does_not_open() {
    let (status, set) = open_set(Some(r#"{"page_size": "B99"}"#));
    assert_eq!(status, Status::InvalidOptions);
    assert!(set_error(set).unwrap().contains("B99"));
    assert_eq!(add_record(set, "<p>x</p>"), Status::RecordSet);
    assert!(set_error(set).unwrap().contains("did not open"));
    assert!(take_output(set).is_empty());
    free_set(set);
}

#[test]
fn record_set_arguments_that_are_null_or_not_utf8_are_invalid() {
    let (_, set) = open_set(None);
    // SAFETY: the set is live, and null HTML with a length is refused before it is read.
    let status = unsafe { rp_record_set_add(set, ptr::null(), 5) };
    assert_eq!(status, Status::InvalidArgument);
    let not_utf8 = [b'<', 0xff, b'>'];
    // SAFETY: the set is live and the HTML is a live slice.
    let status = unsafe { rp_record_set_add(set, not_utf8.as_ptr().cast(), not_utf8.len()) };
    assert_eq!(status, Status::InvalidArgument);
    assert!(set_error(set).unwrap().contains("UTF-8"));
    // The set is as it was.
    assert_eq!(add_record(set, "<p>x</p>"), Status::Ok);
    free_set(set);

    // SAFETY: null sets and a null `out` are refused before they are read.
    unsafe {
        assert_eq!(
            rp_record_set_open(ptr::null(), ptr::null_mut()),
            Status::InvalidArgument
        );
        assert_eq!(
            rp_record_set_add(ptr::null_mut(), c"x".as_ptr(), 1),
            Status::InvalidArgument
        );
        assert_eq!(
            rp_record_set_finish(ptr::null_mut()),
            Status::InvalidArgument
        );
        assert!(rp_record_set_take_output(ptr::null_mut(), ptr::null_mut()).is_null());
        assert!(rp_record_set_warnings_json(ptr::null(), ptr::null_mut()).is_null());
        assert_eq!(rp_record_set_pages(ptr::null()), 0);
        rp_record_set_free(ptr::null_mut());
    }
    let mut set: *mut RecordSet = ptr::null_mut();
    // SAFETY: a null request is refused, and `set` is writable.
    let status = unsafe { rp_record_set_open(ptr::null(), &mut set) };
    assert_eq!(status, Status::InvalidArgument);
    assert_eq!(set_error(set).as_deref(), Some("request is null"));
    free_set(set);
}

#[test]
fn a_record_set_freed_in_the_middle_ends_its_thread() {
    // Freeing joins the set's thread, so a thread that was left behind would hang this test,
    // and one set after another would run out of threads.
    for _ in 0..50 {
        let (_, set) = open_set(None);
        assert_eq!(add_record(set, "<p>x</p>"), Status::Ok);
        free_set(set);
    }
}

#[test]
fn a_record_set_moves_between_threads() {
    let (_, set) = open_set(None);
    let set = set as usize;
    for i in 0..3 {
        thread::spawn(move || {
            assert_eq!(
                add_record(set as *mut RecordSet, &format!("<p>{i}</p>")),
                Status::Ok
            );
        })
        .join()
        .unwrap();
    }
    let set = set as *mut RecordSet;
    // SAFETY: the set is live, and no other thread is using it.
    assert_eq!(unsafe { rp_record_set_finish(set) }, Status::Ok);
    assert_eq!(pages(&take_output(set)).len(), 3);
    free_set(set);
}

#[test]
fn the_version_is_the_workspace_version() {
    // SAFETY: the version is a static NUL-terminated string.
    let version = unsafe { CStr::from_ptr(rp_version()) };
    assert_eq!(version.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
}
