//! Record sets: documents printed one after another as one PDF file.

mod common;

use std::sync::mpsc;
use std::time::Duration;

use common::{
    font_count, font_face, font_file, image_count, page_count, paragraphs, png, web_fonts,
};
use rustyprint::{Error, Limit, Limits, RecordSet, RenderOptions, Summary, render};

/// The text of each page of a PDF file.
fn page_texts(pdf: &[u8]) -> Vec<String> {
    let document = lopdf::Document::load_mem(pdf).expect("valid PDF");
    let pages = document.get_pages();
    pages
        .keys()
        .map(|&number| document.extract_text(&[number]).expect("the page has text"))
        .collect()
}

/// Margin boxes that tell a page's number, the page count, its side and whether it is a
/// first page.
const FOLIOS: &str = "
    @page { @bottom-center { content: 'folio' counter(page) 'of' counter(pages) 'end' } }
    @page :left { @top-left { content: 'leftside' } }
    @page :right { @top-left { content: 'rightside' } }
    @page :first { @top-right { content: 'firstpage' } }
";

fn with_folios() -> RenderOptions {
    RenderOptions {
        stylesheet: Some(FOLIOS.to_owned()),
        ..RenderOptions::default()
    }
}

#[test]
fn records_added_one_by_one_are_one_file_with_pages_numbered_through() {
    let records = [
        "<h1>Alpha record</h1>".to_owned(),
        format!("<h1>Bravo record</h1>{}", paragraphs(60)),
        "<h1>Charlie record</h1>".to_owned(),
    ];
    let options = with_folios();
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    let mut added = Vec::new();
    for record in &records {
        let record_added = set.add(record).unwrap();
        assert_eq!(record_added.warnings, Vec::<String>::new());
        added.push(record_added.pages);
        assert_eq!(set.pages(), added.iter().sum::<usize>());
    }
    // Each record takes the pages it takes as a document of its own.
    let alone: Vec<usize> = records
        .iter()
        .map(|record| page_count(&render(record, &options).unwrap().pdf))
        .collect();
    assert_eq!(added, alone);
    assert_eq!(added[0], 1);
    assert!(added[1] > 2, "the second record has {} pages", added[1]);

    let (pdf, summary) = set.finish().unwrap();
    let total: usize = added.iter().sum();
    assert_eq!(
        summary,
        Summary {
            pages: total,
            records: 3
        }
    );
    let texts = page_texts(&pdf);
    assert_eq!(texts.len(), total);

    // The records are in order, each from a new page.
    let starts = [0, added[0], added[0] + added[1]];
    for (name, start) in ["Alpha", "Bravo", "Charlie"].into_iter().zip(starts) {
        let on: Vec<usize> = (0..total).filter(|&p| texts[p].contains(name)).collect();
        assert_eq!(on, [start], "{name} record");
    }

    for (index, text) in texts.iter().enumerate() {
        // `counter(page)` runs through the set, and `counter(pages)` is the pages of the set.
        let folio = format!("folio{}of{total}end", index + 1);
        assert!(text.contains(&folio), "page {}: {text:?}", index + 1);
        // Left and right pages carry on through the set: the first page is a right page.
        let side = if index % 2 == 0 {
            "rightside"
        } else {
            "leftside"
        };
        assert!(text.contains(side), "page {}: {text:?}", index + 1);
        // The first page of each record is a `:first` page.
        assert_eq!(
            text.contains("firstpage"),
            starts.contains(&index),
            "page {}: {text:?}",
            index + 1
        );
    }
}

#[test]
fn an_image_every_record_has_is_written_once() {
    let mut options = RenderOptions::default();
    options.assets.files.insert("logo.png".into(), png(40, 20));
    for number in 0..4 {
        let photo = png(10 + number, 10);
        options
            .assets
            .files
            .insert(format!("photo{number}.png"), photo);
    }
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    for number in 0..4 {
        let record =
            format!("<img src='logo.png'><p>Record {number}</p><img src='photo{number}.png'>");
        let added = set.add(&record).unwrap();
        assert_eq!(added.warnings, Vec::<String>::new());
    }
    let (pdf, _) = set.finish().unwrap();
    assert_eq!(page_count(&pdf), 4);
    // The logo once, and each of the four photos.
    assert_eq!(image_count(&pdf), 5);
}

#[test]
fn the_records_of_a_set_draw_their_pictures_within_one_limit() {
    // A gradient is drawn to pixels: each record's drawing is 160 × 40 of them.
    let record = |number: usize| {
        format!(
            "<p>Record {number}</p><svg width='40' height='10'>\
             <linearGradient id='paint'><stop offset='0' stop-color='#ff00{:02x}'/>\
             <stop offset='1' stop-color='#0000ff'/></linearGradient>\
             <rect width='40' height='10' fill='url(#paint)'/></svg>",
            20 * number
        )
    };
    let mut options = RenderOptions::default();
    options.limits.max_svg_pixels = 3 * 160 * 40;
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    for number in 0..7 {
        let added = set.add(&record(number)).unwrap();
        // The first three fit; each record after them says that its own was left blank.
        if number < 3 {
            assert_eq!(added.warnings, Vec::<String>::new(), "record {number}");
        } else {
            assert_eq!(added.warnings.len(), 1, "{:?}", added.warnings);
            assert!(
                added.warnings[0].contains("an SVG was left blank"),
                "{:?}",
                added.warnings
            );
        }
    }
    let (pdf, _) = set.finish().unwrap();
    assert_eq!(page_count(&pdf), 7);
    assert_eq!(image_count(&pdf), 3);
}

/// How many times larger a set may be than its content printed whole: a set writes the
/// resources of every page as an object of their own.
const MAX_SET_SIZE: f64 = 1.15;

#[test]
fn the_web_fonts_of_the_stylesheet_are_embedded_once_for_the_set() {
    let section = |number: usize| {
        format!(
            "<section><h1>Record {number}</h1>\
             <p>Plain text and <b>bold text</b> of record {number}.</p></section>"
        )
    };
    let options = with_stylesheet(&format!(
        "{} section + section {{ break-before: page }}",
        web_fonts()
    ));
    let whole: String = (0..200).map(section).collect();
    let whole = render(&whole, &options).unwrap();
    assert_eq!(whole.warnings, Vec::<String>::new());
    assert_eq!(page_count(&whole.pdf), 200);
    // The regular and the bold.
    assert_eq!(font_count(&whole.pdf), 2);

    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    for number in 0..200 {
        let added = set.add(&section(number)).unwrap();
        assert_eq!(added.warnings, Vec::<String>::new());
    }
    let (pdf, _) = set.finish().unwrap();
    assert_eq!(page_count(&pdf), 200);
    assert_eq!(font_count(&pdf), 2);
    let ratio = pdf.len() as f64 / whole.pdf.len() as f64;
    assert!(ratio < MAX_SET_SIZE, "{ratio} times the whole print");
}

#[test]
fn a_font_file_is_one_font_of_the_set_wherever_a_record_has_it_from() {
    let mut options = with_stylesheet(&format!(
        "{} p {{ font-family: Doc }} @font-face {{ font-family: Asset; font-weight: 700; src: url(bold.ttf) }}",
        font_face("NotoSansMono-Regular.ttf", 400)
    ));
    let bold = font_file("NotoSansMono-Bold.ttf");
    options.assets.files.insert("bold.ttf".into(), bold);
    // The same two files again: in a rule of the record, and as a file the record links.
    let own = format!(
        "<style>{} {} h1 {{ font: 400 20px Own }} h2 {{ font: 400 16px Asset }}</style>",
        font_face("NotoSansMono-Regular.ttf", 400).replace("Doc", "Own"),
        "@font-face { font-family: Own; font-weight: 700; src: url(./bold.ttf?again) }"
    );
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    for number in 0..6 {
        let style = if number % 2 == 1 { &own } else { "" };
        let record =
            format!("{style}<h1>Record <b>{number}</b></h1><h2>In the asset</h2><p>Its text</p>");
        let added = if number == 4 {
            set.add_continuing(&record)
        } else {
            set.add(&record)
        };
        assert_eq!(added.unwrap().warnings, Vec::<String>::new());
    }
    let (pdf, _) = set.finish().unwrap();
    // The regular, the bold, and the bundled font of the headings without the rules.
    assert_eq!(font_count(&pdf), 3);
}

#[test]
fn the_writer_has_each_record_when_it_is_added() {
    let mut set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    assert!(set.writer_mut().is_empty());
    let mut file = Vec::new();
    for record in ["<p>One</p>", "<p>Two</p>", "<p>Three</p>"] {
        set.add(record).unwrap();
        // What the writer holds can be taken after every record.
        let part = std::mem::take(set.writer_mut());
        assert!(!part.is_empty(), "nothing was written for {record}");
        file.extend(part);
    }
    assert!(file.starts_with(b"%PDF-"));
    let (rest, summary) = set.finish().unwrap();
    assert_eq!(summary.pages, 3);
    file.extend(rest);
    let texts = page_texts(&file);
    assert_eq!(texts.len(), 3);
    assert!(texts[0].contains("One") && texts[2].contains("Three"));
}

#[test]
fn a_set_without_records_is_a_file_without_pages() {
    let set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    let (pdf, summary) = set.finish().unwrap();
    assert_eq!(
        summary,
        Summary {
            pages: 0,
            records: 0
        }
    );
    assert_eq!(page_count(&pdf), 0);
}

fn exceeds<T>(result: Result<T, Error>, expected: Limit, expected_max: u64) {
    match result {
        Err(Error::LimitExceeded { limit, max }) => {
            assert_eq!((limit, max), (expected, expected_max))
        }
        Err(other) => panic!("expected the {expected} limit, got {other}"),
        Ok(_) => panic!("expected the {expected} limit, but the record was added"),
    }
}

fn failed_earlier<T>(result: Result<T, Error>) {
    match result {
        Err(Error::RecordSet(message)) => {
            assert!(message.contains("an earlier record failed"), "{message}")
        }
        Err(other) => panic!("expected a failed set, got {other}"),
        Ok(_) => panic!("expected a failed set, but the call succeeded"),
    }
}

fn with_limits(limits: Limits) -> RenderOptions {
    RenderOptions {
        limits,
        ..RenderOptions::default()
    }
}

#[test]
fn a_record_over_the_page_limit_fails_the_set() {
    let options = with_limits(Limits {
        max_pages: 2,
        ..Limits::default()
    });
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    // The limit is each record's: two records of one page are within it.
    assert_eq!(set.add("<p>One</p>").unwrap().pages, 1);
    assert_eq!(set.add("<p>Two</p>").unwrap().pages, 1);
    exceeds(set.add(&paragraphs(200)), Limit::Pages, 2);
    assert_eq!(set.pages(), 2);
    failed_earlier(set.add("<p>Three</p>"));
    failed_earlier(set.finish());
}

#[test]
fn a_set_over_the_total_page_limit_fails() {
    let options = with_limits(Limits {
        max_total_pages: 2,
        ..Limits::default()
    });
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    set.add("<p>One</p>").unwrap();
    set.add("<p>Two</p>").unwrap();
    exceeds(set.add("<p>Three</p>"), Limit::TotalPages, 2);
    failed_earlier(set.add("<p>Four</p>"));
}

#[test]
fn a_record_over_the_input_size_or_past_the_deadline_fails_the_set() {
    let options = with_limits(Limits {
        max_input_bytes: 100,
        ..Limits::default()
    });
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    set.add("<p>Short</p>").unwrap();
    exceeds(set.add(&paragraphs(5)), Limit::InputSize, 100);
    failed_earlier(set.add("<p>Short</p>"));

    let options = with_limits(Limits {
        timeout: Some(Duration::ZERO),
        ..Limits::default()
    });
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    assert!(matches!(set.add(&paragraphs(50)), Err(Error::Timeout)));
    failed_earlier(set.add("<p>Short</p>"));
}

#[test]
fn a_panic_in_a_record_is_an_error_that_fails_the_set() {
    let mut set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    set.add("<p>One</p>").unwrap();
    let panicked = set.add("<p>Two</p><!--rustyprint:panic-->");
    assert!(matches!(panicked, Err(Error::Internal(_))), "{panicked:?}");
    failed_earlier(set.add("<p>Three</p>"));
}

#[test]
fn an_invalid_scale_does_not_open_a_set() {
    let options = RenderOptions {
        scale: 5.0,
        ..RenderOptions::default()
    };
    let opened = RecordSet::open(Vec::new(), &options);
    assert!(matches!(opened, Err(Error::InvalidOptions(_))));
}

#[test]
fn a_set_goes_to_another_thread_and_is_dropped_unfinished_without_hanging() {
    let mut set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    set.add("<p>One</p>").unwrap();
    let unopened = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    let (dropped, done) = mpsc::channel();
    std::thread::spawn(move || {
        set.add("<p>Two</p>").unwrap();
        drop(set);
        drop(unopened);
        dropped.send(()).unwrap();
    });
    done.recv_timeout(Duration::from_secs(30))
        .expect("the sets are dropped");
}

/// How many content streams each page of a PDF file has.
fn content_streams(pdf: &[u8]) -> Vec<usize> {
    let document = lopdf::Document::load_mem(pdf).expect("valid PDF");
    let pages = document.get_pages();
    pages
        .values()
        .map(|&id| document.get_page_contents(id).len())
        .collect()
}

/// The text of each page from its last `word` on, without its white space.
fn from_word(pdf: &[u8], word: &str) -> Vec<String> {
    page_texts(pdf)
        .iter()
        .map(|text| {
            let text: String = text.split_whitespace().collect();
            let at = text.rfind(word).unwrap_or(text.len());
            text[at..].to_owned()
        })
        .collect()
}

const PAGE_OF_PAGES: &str =
    r#"@page { @bottom-right { content: "Page " counter(page) " of " counter(pages) } }"#;

fn with_stylesheet(stylesheet: &str) -> RenderOptions {
    RenderOptions {
        stylesheet: Some(stylesheet.to_owned()),
        ..RenderOptions::default()
    }
}

/// Four records of one, several, two and many pages.
fn four_records() -> [String; 4] {
    [
        "<h1>Alpha record</h1>".to_owned(),
        format!("<h1>Bravo record</h1>{}", paragraphs(60)),
        format!("<h1>Charlie record</h1>{}", paragraphs(25)),
        format!("<h1>Delta record</h1>{}", paragraphs(130)),
    ]
}

#[test]
fn the_page_count_in_a_margin_is_that_of_the_whole_set() {
    let options = with_stylesheet(PAGE_OF_PAGES);
    let records = four_records();
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    let mut added = Vec::new();
    let mut file = Vec::new();
    for record in &records {
        let record_added = set.add(record).unwrap();
        assert_eq!(record_added.warnings, Vec::<String>::new());
        added.push(record_added.pages);
        // The pages are written with their record, before the total is known.
        let part = std::mem::take(set.writer_mut());
        let written = part
            .windows(9)
            .filter(|bytes| bytes == b"/Contents")
            .count();
        assert_eq!(written, record_added.pages, "page objects in the writer");
        file.extend(part);
    }
    let mut lengths = added.clone();
    lengths.sort_unstable();
    lengths.dedup();
    assert_eq!(lengths.len(), 4, "the records have {added:?} pages");
    let (rest, summary) = set.finish().unwrap();
    let pdf = [file, rest].concat();
    let total = summary.pages;
    assert_eq!(total, added.iter().sum::<usize>());
    assert!(total >= 10, "the total of {total} has two digits");

    let expected: Vec<String> = (1..=total).map(|k| format!("Page{k}of{total}")).collect();
    assert_eq!(from_word(&pdf, "Page"), expected);
    // Every page has the stream of its footer after its own.
    assert_eq!(content_streams(&pdf), vec![2; total]);

    // The same records as one document, each from a new page, have the same footers.
    let whole: String = records
        .iter()
        .map(|record| format!("<div style='break-before: page'>{record}</div>"))
        .collect();
    let rendered = render(&whole, &options).unwrap().pdf;
    assert_eq!(from_word(&rendered, "Page"), expected);
    // There the total is known from the start, and nothing waits for it.
    assert_eq!(content_streams(&rendered), vec![1; total]);
}

#[test]
fn only_a_page_that_shows_the_page_count_gets_a_second_stream() {
    let print = |stylesheet: &str| {
        let mut set = RecordSet::open(Vec::new(), &with_stylesheet(stylesheet)).unwrap();
        let mut starts = Vec::new();
        for record in &four_records() {
            starts.push(set.pages());
            set.add(record).unwrap();
        }
        (set.finish().unwrap().0, starts)
    };
    // Without the page count nothing waits.
    let (pdf, _) = print("@page { @bottom-right { content: 'Page ' counter(page) } }");
    let total = page_count(&pdf);
    assert_eq!(content_streams(&pdf), vec![1; total]);
    let expected: Vec<String> = (1..=total).map(|k| format!("Page{k}")).collect();
    assert_eq!(from_word(&pdf, "Page"), expected);

    // With it on first pages alone, only those wait, and the others show their number
    // as before.
    let (pdf, starts) = print(
        "@page { @bottom-right { content: 'Page ' counter(page) } }
         @page :first { @bottom-right { content: 'Page ' counter(page) ' of ' counter(pages) } }",
    );
    let streams: Vec<usize> = (0..total)
        .map(|page| if starts.contains(&page) { 2 } else { 1 })
        .collect();
    assert_eq!(content_streams(&pdf), streams);
    let expected: Vec<String> = (0..total)
        .map(|page| match starts.contains(&page) {
            true => format!("Page{}of{total}", page + 1),
            false => format!("Page{}", page + 1),
        })
        .collect();
    assert_eq!(from_word(&pdf, "Page"), expected);
}

#[test]
fn left_and_right_pages_show_their_own_page_count_footer() {
    let options = with_stylesheet(
        "@page :left { @bottom-left { content: 'Verso ' counter(pages) ' has ' counter(page) } }
         @page :right { @bottom-right { content: 'Recto ' counter(page) ' in ' counter(pages) } }",
    );
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    for record in &four_records() {
        set.add(record).unwrap();
    }
    let (pdf, summary) = set.finish().unwrap();
    let total = summary.pages;
    let texts = page_texts(&pdf);
    assert_eq!(texts.len(), total);
    for (index, text) in texts.iter().enumerate() {
        let text: String = text.split_whitespace().collect();
        let number = index + 1;
        // The first page of the set is a right page.
        let (shown, other) = match index % 2 {
            0 => (format!("Recto{number}in{total}"), "Verso"),
            _ => (format!("Verso{total}has{number}"), "Recto"),
        };
        assert!(text.ends_with(&shown), "page {number}: {text:?}");
        assert!(!text.contains(other), "page {number}: {text:?}");
    }
}

#[test]
fn a_running_element_beside_the_page_count_is_kept_for_it() {
    // The two boxes share the bottom edge by what each holds, so the running element
    // waits for the total as well.
    let options = with_stylesheet(
        "@page {
            @bottom-left { content: element(footer) }
            @bottom-right { content: 'Sheet ' counter(page) ' of ' counter(pages) }
            @top-center { content: 'Heading' counter(page) }
         }
         .footer { position: running(footer) }",
    );
    let records = four_records();
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    let mut owners = Vec::new();
    for (name, record) in ["Alpha", "Bravo", "Charlie", "Delta"]
        .into_iter()
        .zip(&records)
    {
        let html = format!("<div class='footer'>Statement{name}</div>{record}");
        let added = set.add(&html).unwrap();
        owners.extend(std::iter::repeat_n(name, added.pages));
    }
    let (pdf, summary) = set.finish().unwrap();
    let total = summary.pages;
    let texts = page_texts(&pdf);
    assert_eq!(texts.len(), total);
    for (index, text) in texts.iter().enumerate() {
        let text: String = text.split_whitespace().collect();
        let number = index + 1;
        assert!(
            text.contains(&format!("Statement{}", owners[index])),
            "page {number}: {text:?}"
        );
        assert!(
            text.contains(&format!("Sheet{number}of{total}")),
            "page {number}: {text:?}"
        );
        // The box of another edge is painted with its page.
        assert!(
            text.contains(&format!("Heading{number}")),
            "page {number}: {text:?}"
        );
    }
    assert_eq!(content_streams(&pdf), vec![2; total]);
}
