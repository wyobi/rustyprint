//! Helpers shared by the integration tests.

#![allow(dead_code)]

use std::io::Cursor;

use lopdf::Object;
use lopdf::content::Content;
use rustyprint::{RecordSet, RenderOptions};

/// CSS pixels per millimetre.
pub const MM: f32 = 96.0 / 25.4;

pub fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

pub fn page_count(pdf: &[u8]) -> usize {
    lopdf::Document::load_mem(pdf)
        .expect("valid PDF")
        .get_pages()
        .len()
}

/// How many image XObjects the PDF holds.
pub fn image_count(pdf: &[u8]) -> usize {
    let doc = lopdf::Document::load_mem(pdf).expect("valid PDF");
    doc.objects
        .values()
        .filter(|object| {
            object.as_stream().is_ok_and(|stream| {
                stream
                    .dict
                    .get(b"Subtype")
                    .and_then(|s| s.as_name())
                    .is_ok_and(|name| name == b"Image")
            })
        })
        .count()
}

/// How many fonts the PDF embeds.
pub fn font_count(pdf: &[u8]) -> usize {
    let doc = lopdf::Document::load_mem(pdf).expect("valid PDF");
    doc.objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .filter(|dict| {
            dict.get(b"Subtype")
                .and_then(|s| s.as_name())
                .is_ok_and(|name| name == b"Type0")
        })
        .count()
}

/// A file of the repository's `fonts` directory.
pub fn font_file(name: &str) -> Vec<u8> {
    let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fonts");
    std::fs::read(fonts.join(name)).expect("the font is in the repository")
}

/// An `@font-face` rule for the family `Doc` at `weight`, with the font file `name` as a
/// `data:` URL.
pub fn font_face(name: &str, weight: u16) -> String {
    format!(
        "@font-face {{ font-family: Doc; font-weight: {weight}; \
         src: url(data:font/ttf;base64,{}) }}",
        base64(&font_file(name))
    )
}

/// A stylesheet that sets the document in a web font, with its regular and its bold as
/// `data:` URLs.
pub fn web_fonts() -> String {
    format!(
        "{}{} body {{ font-family: Doc }}",
        font_face("NotoSansMono-Regular.ttf", 400),
        font_face("NotoSansMono-Bold.ttf", 700)
    )
}

pub fn base64(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |group, (at, &byte)| {
            group | u32::from(byte) << (16 - 8 * at)
        });
        for digit in 0..4 {
            if digit <= chunk.len() {
                text.push(DIGITS[(group >> (18 - 6 * digit)) as usize & 63] as char);
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// `count` paragraphs of filler text.
pub fn paragraphs(count: usize) -> String {
    (0..count)
        .map(|i| {
            format!(
                "<p>Paragraph {i}. The quick brown fox jumps over the lazy dog, and then \
                 it jumps back again, because the dog never minded.</p>"
            )
        })
        .collect()
}

/// A solid PNG of `width` by `height` pixels.
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30]));
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("PNG encodes");
    bytes
}

/// Documents that between them exercise most of the engine, including the parts with
/// per-thread caches: calc, grid, writing modes, filters and rasterised effects.
pub fn varied_documents() -> Vec<String> {
    vec![
        "<h1>Title</h1><p>Hello <b>bold</b> and <i>italic</i> world.</p>".to_owned(),
        "<div style='display: grid; grid-template-columns: repeat(3, 1fr); gap: calc(1em + 2px)'>\
         <div>a</div><div>b</div><div>c</div><div>d</div></div>"
            .to_owned(),
        "<p style='writing-mode: vertical-rl; height: 5cm'>縦書きの文章です。</p>\
         <ol><li>one</li><li>two</li></ol>"
            .to_owned(),
        "<div style='filter: blur(2px); background: linear-gradient(red, blue); \
         width: 4cm; height: 2cm; box-shadow: 0 0 8px black'>Effects</div>"
            .to_owned(),
        format!(
            "<table border=1>{}</table>",
            "<tr><td>cell</td><td>cell</td></tr>".repeat(80)
        ),
    ]
}

/// What a page paints: every operation of its content, in order.
pub type Painted = Vec<(String, Vec<Object>)>;

/// The text and the content of each page of a PDF file.
pub fn pages(pdf: &[u8]) -> Vec<(String, Painted)> {
    let document = lopdf::Document::load_mem(pdf).expect("valid PDF");
    let pages = document.get_pages();
    pages
        .iter()
        .map(|(&number, &id)| {
            let text = document.extract_text(&[number]).expect("the page has text");
            let content = document.get_page_content(id);
            let content = Content::decode(&content).expect("the content decodes");
            let painted = content.operations.into_iter();
            (text, painted.map(|o| (o.operator, o.operands)).collect())
        })
        .collect()
}

pub fn number(object: &Object) -> Option<f32> {
    match object {
        Object::Integer(value) => Some(*value as f32),
        Object::Real(value) => Some(*value),
        _ => None,
    }
}

/// Whether two operands are the same, with numbers no further apart than half a point.
/// Shown text is as many glyphs: a font's glyphs are numbered in the order a file first
/// uses them, which is not the same in a file whose pages are painted a piece at a time,
/// and what the glyphs read is compared as the text of the page.
pub fn same(a: &Object, b: &Object) -> bool {
    match (number(a), number(b), a, b) {
        (Some(a), Some(b), ..) => (a - b).abs() <= 0.5,
        (_, _, Object::String(a, _), Object::String(b, _)) => a.len() == b.len(),
        (_, _, Object::Array(a), Object::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
        }
        _ => a == b,
    }
}

/// Fails unless `pieces` is `whole`: the same pages, with the same text and the same
/// content on each, every box, border, background and line of text in the same place
/// within half a point.
pub fn assert_same_print(pieces: &[u8], whole: &[u8]) {
    let (pieces, whole) = (pages(pieces), pages(whole));
    assert_eq!(pieces.len(), whole.len(), "pages");
    for (index, ((text, painted), (whole_text, whole_painted))) in
        pieces.iter().zip(&whole).enumerate()
    {
        let page = index + 1;
        assert_eq!(text, whole_text, "the text of page {page}");
        assert_eq!(
            painted.len(),
            whole_painted.len(),
            "how much page {page} paints"
        );
        for (at, (a, b)) in painted.iter().zip(whole_painted).enumerate() {
            let alike = a.0 == b.0
                && a.1.len() == b.1.len()
                && a.1.iter().zip(&b.1).all(|(a, b)| same(a, b));
            assert!(alike, "page {page}, operation {at}: {a:?} is not {b:?}");
        }
    }
}

/// Prints `pieces` as one flow: the first as a record, the others continuing it.
pub fn print_pieces(pieces: &[String], options: &RenderOptions) -> Vec<u8> {
    let mut set = RecordSet::open(Vec::new(), options).unwrap();
    for (index, piece) in pieces.iter().enumerate() {
        let added = if index == 0 {
            set.add(piece)
        } else {
            set.add_continuing(piece)
        };
        assert_eq!(
            added.unwrap().warnings,
            Vec::<String>::new(),
            "piece {index}"
        );
    }
    let (pdf, summary) = set.finish().unwrap();
    assert_eq!(summary.pages, page_count(&pdf));
    assert_eq!(summary.records, pieces.len());
    pdf
}
