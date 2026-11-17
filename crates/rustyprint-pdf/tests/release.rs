//! A session lets go of each batch once it is written. This test has a test binary to
//! itself, so that no other test's memory is counted.

use std::sync::Arc;

use rustyprint_layout::display::Document;
use rustyprint_layout::{DisplayItem, Image, ImageData, Page, Rect, Size};

/// Batches written, each one page with an image no other batch has.
const BATCHES: usize = 300;

/// The side of each image in pixels: about one megabyte of RGB samples.
const SIDE: u32 = 600;

/// The most the process may grow while it writes: a small part of the 300 MB of pixels.
const MAX_GROWTH_KB: usize = 60 * 1024;

/// How many batches go by between two readings of the process's memory.
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

/// An image whose samples no other number gives.
fn image(number: usize) -> Arc<Image> {
    let mut samples = vec![(number % 251) as u8; (SIDE * SIDE * 3) as usize];
    samples[..8].copy_from_slice(&(number as u64).to_be_bytes());
    Arc::new(Image {
        width: SIDE,
        height: SIDE,
        data: ImageData::Pixels {
            samples,
            gray: false,
            alpha: None,
        },
        orientation: 1,
        resolution: None,
        natural: None,
        shape_only: false,
        fit: None,
    })
}

#[test]
fn a_session_holds_no_pixels_of_the_batches_it_wrote() {
    let setup = Document::default();
    let mut session =
        rustyprint_pdf::Session::new(std::io::sink(), rustyprint_pdf::Setup::of(&setup)).unwrap();
    let before = resident_kb();
    let mut peak = before;
    for number in 0..BATCHES {
        let image = image(number);
        let pixels = Arc::downgrade(&image);
        let item = DisplayItem::Image {
            image,
            rect: Rect::new(0.0, 0.0, 100.0, 100.0),
            smooth: true,
            orientation: 1,
            alpha: 255,
        };
        let batch = Document {
            pages: vec![Page::new(Size::A4, vec![item])],
            ..Document::default()
        };
        session.add_pages(&batch).unwrap();
        drop(batch);
        // The batch held the only image; the session keeps none of it.
        assert!(pixels.upgrade().is_none(), "batch {number} is still held");
        if number % SAMPLE_EVERY == 0 || number + 1 == BATCHES {
            peak = peak.max(resident_kb());
        }
    }
    assert_eq!(session.page_count(), BATCHES);
    let growth = peak.saturating_sub(before);
    assert!(
        growth < MAX_GROWTH_KB,
        "the process grew by {growth} KB while writing {BATCHES} images of about 1 MB"
    );
    session.finish().unwrap();
}
