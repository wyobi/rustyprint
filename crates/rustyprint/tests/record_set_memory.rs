//! A record set lets go of each record once it is written. This test has a test binary to
//! itself, so that no other test's memory is counted.

mod common;

use common::base64;
use rustyprint::{RecordSet, RenderOptions};

/// Records printed, each with an image no other record has.
const RECORDS: usize = 200;

/// The side of each image in pixels: about one megabyte of RGB samples.
const SIDE: u32 = 600;

/// The most the process may grow while it prints: a small part of the 200 MB of pixels.
const MAX_GROWTH_KB: usize = 60 * 1024;

/// How many records go by between two readings of the process's memory.
const SAMPLE_EVERY: usize = 10;

/// The memory the process holds, in kilobytes.
fn resident_kb() -> usize {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("ps prints the resident set size")
}

/// An uncompressed 24-bit BMP file whose pixels no other number gives.
fn bitmap(number: usize) -> Vec<u8> {
    let pixels = (SIDE * SIDE * 3) as usize;
    let mut file = Vec::with_capacity(54 + pixels);
    file.extend(b"BM");
    file.extend((54 + pixels as u32).to_le_bytes());
    file.extend([0; 4]);
    file.extend(54u32.to_le_bytes());
    file.extend(40u32.to_le_bytes());
    file.extend(SIDE.to_le_bytes());
    file.extend(SIDE.to_le_bytes());
    file.extend(1u16.to_le_bytes());
    file.extend(24u16.to_le_bytes());
    file.extend([0; 24]);
    file.resize(54 + pixels, (number % 251) as u8);
    file[54..62].copy_from_slice(&(number as u64).to_be_bytes());
    file
}

fn record(number: usize) -> String {
    format!(
        "<p>Record {number}</p><img src='data:image/bmp;base64,{}'>",
        base64(&bitmap(number))
    )
}

#[test]
fn a_set_holds_no_pixels_of_the_records_it_printed() {
    // The image of a record is one the engine decodes and writes.
    let mut checked = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    checked.add(&record(RECORDS)).unwrap();
    assert_eq!(common::image_count(&checked.finish().unwrap().0), 1);

    // Every page waits for the page count of the set, and still holds nothing of its record.
    let options = RenderOptions {
        stylesheet: Some(
            "@page { @bottom-right { content: 'Page ' counter(page) ' of ' counter(pages) } }"
                .to_owned(),
        ),
        ..RenderOptions::default()
    };
    let mut set = RecordSet::open(std::io::sink(), &options).unwrap();
    // The first record loads the fonts, which stay for the whole set.
    set.add("<p>Record</p>").unwrap();
    let before = resident_kb();
    let mut peak = before;
    for number in 0..RECORDS {
        let added = set.add(&record(number)).unwrap();
        assert_eq!(added.warnings, Vec::<String>::new());
        if number % SAMPLE_EVERY == 0 || number + 1 == RECORDS {
            peak = peak.max(resident_kb());
        }
    }
    let growth = peak.saturating_sub(before);
    assert!(
        growth < MAX_GROWTH_KB,
        "the process grew by {growth} KB while printing {RECORDS} images of about 1 MB"
    );
    let (_, summary) = set.finish().unwrap();
    assert_eq!(summary.records, RECORDS + 1);
    assert!(summary.pages > RECORDS);
}
