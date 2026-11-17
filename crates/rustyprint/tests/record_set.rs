//! Record sets: documents printed one after another as one PDF file.

mod common;

use std::sync::mpsc;
use std::time::Duration;

use common::{image_count, page_count, paragraphs, png};
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
        // `counter(page)` runs through the set; `counter(pages)` is the record's own.
        let record = starts.iter().rposition(|&start| start <= index).unwrap();
        let folio = format!("folio{}of{}end", index + 1, added[record]);
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
