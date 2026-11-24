//! Chinese and Arabic text in the PDF, with the bundled fonts: copied text gives the
//! original characters; the marks `text-overflow` adds fall back to other fonts as text does.

mod common;

use common::{base64, font_file};
use rustyprint::{RenderOptions, render};

/// The text of the PDF's first page in content order, as a viewer copies it: a marked
/// content span's `/ActualText` stands for its glyphs, and other glyphs give the text of
/// their font's ToUnicode map.
fn copied_text(body: &str) -> String {
    use lopdf::content::Content;
    use lopdf::{Document, Object};

    let html = format!("<!DOCTYPE html><meta charset=utf-8><body>{body}");
    let output = render(&html, &RenderOptions::default()).expect("renders");
    assert!(output.warnings.is_empty(), "{:?}", output.warnings);
    let pdf = Document::load_mem(&output.pdf).expect("valid PDF");
    let page = pdf.get_pages()[&1];
    let fonts = pdf.get_page_fonts(page).expect("page fonts");
    let content = Content::decode(&pdf.get_page_content(page)).expect("decodes");

    let utf16 = |bytes: &[u8]| {
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    };
    // CID to text, from each font's ToUnicode CMap of `<cid> <utf16>` pairs.
    let to_unicode = |font: &[u8]| -> Vec<(u16, String)> {
        let dict = fonts[font];
        let stream = pdf
            .get_object(dict.get(b"ToUnicode").unwrap().as_reference().unwrap())
            .unwrap()
            .as_stream()
            .unwrap();
        let cmap = String::from_utf8(stream.decompressed_content().unwrap()).unwrap();
        let hex = |h: &str| -> Vec<u8> {
            (0..h.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
                .collect()
        };
        let mut pairs = Vec::new();
        for line in cmap.lines() {
            let parts: Vec<&str> = line
                .split(['<', '>', ' '])
                .filter(|p| !p.is_empty())
                .collect();
            if let [cid, text] = parts[..]
                && cid.len() == 4
                && cid.bytes().all(|b| b.is_ascii_hexdigit())
            {
                let cid = u16::from_str_radix(cid, 16).unwrap();
                pairs.push((cid, utf16(&hex(text))));
            }
        }
        pairs
    };

    let mut text = String::new();
    let mut map = Vec::new();
    let mut actual_text = 0;
    for op in &content.operations {
        match op.operator.as_str() {
            "Tf" => map = to_unicode(op.operands[0].as_name().unwrap()),
            "BDC" => {
                let spans = |o: &Object| o.as_dict().ok()?.get(b"ActualText").ok().cloned();
                if let Some(Object::String(bytes, _)) = op.operands.get(1).and_then(spans) {
                    text += &utf16(&bytes[2..]);
                    actual_text += 1;
                } else if actual_text > 0 {
                    actual_text += 1;
                }
            }
            "EMC" if actual_text > 0 => actual_text -= 1,
            "TJ" | "Tj" if actual_text == 0 => {
                let strings: Vec<&Object> = match &op.operands[0] {
                    Object::Array(items) => items.iter().collect(),
                    other => vec![other],
                };
                for string in strings {
                    if let Object::String(bytes, _) = string {
                        for pair in bytes.chunks(2) {
                            let cid = u16::from_be_bytes([pair[0], pair[1]]);
                            if let Some((_, t)) = map.iter().find(|(c, _)| *c == cid) {
                                text += t;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    text
}

/// The base fonts the PDF embeds.
fn embedded_fonts(body: &str) -> Vec<String> {
    let html = format!("<!DOCTYPE html><meta charset=utf-8><body>{body}");
    let output = render(&html, &RenderOptions::default()).expect("renders");
    let pdf = lopdf::Document::load_mem(&output.pdf).expect("valid PDF");
    pdf.objects
        .values()
        .filter_map(|object| {
            let dict = object.as_dict().ok()?;
            if dict.get(b"Type").ok()?.as_name().ok()? != b"FontDescriptor" {
                return None;
            }
            let name = dict.get(b"FontName").ok()?.as_name().ok()?;
            Some(String::from_utf8_lossy(name).into_owned())
        })
        .collect()
}

#[cfg(feature = "cjk")]
#[test]
fn chinese_copies_as_written() {
    let text = "简体中文排版，汉字与标点。";
    assert_eq!(copied_text(&format!("<p>{text}</p>")), text);
    let fonts = embedded_fonts(&format!("<p>{text}</p>"));
    assert!(fonts.iter().any(|f| f.contains("NotoSansSC")), "{fonts:?}");
}

#[test]
fn arabic_copies_as_written() {
    let text = "مرحبا بالعالم";
    // Glyphs are in visual order, so right-to-left words copy reversed, as from other PDF
    // generators; viewers reorder them. The letters must be the originals, not presentation
    // forms, with nothing left over from the dots drawn as glyphs of their own.
    let copied = copied_text(&format!("<p dir=rtl>{text}</p>"));
    let reversed: String = text.chars().rev().collect();
    assert_eq!(copied, reversed);
    let fonts = embedded_fonts(&format!("<p>{text}</p>"));
    assert!(
        fonts.iter().any(|f| f.contains("NotoNaskhArabic")),
        "{fonts:?}"
    );
}

#[test]
fn latin_documents_embed_no_script_fonts() {
    let fonts = embedded_fonts("<p>Hello world</p>");
    assert!(
        fonts
            .iter()
            .all(|f| !f.contains("Arabic") && !f.contains("NotoSansSC")),
        "{fonts:?}"
    );
}

/// Glyphs the first page draws with the missing-glyph box, CID 0.
fn missing_glyphs(body: &str) -> usize {
    let html = format!("<!DOCTYPE html><meta charset=utf-8><body>{body}");
    let output = render(&html, &RenderOptions::default()).expect("renders");
    let pdf = lopdf::Document::load_mem(&output.pdf).expect("valid PDF");
    let page = pdf.get_pages()[&1];
    let content = lopdf::content::Content::decode(&pdf.get_page_content(page)).unwrap();
    let strings = content
        .operations
        .iter()
        .filter(|op| matches!(op.operator.as_str(), "TJ" | "Tj"))
        .flat_map(|op| match &op.operands[0] {
            lopdf::Object::Array(items) => items.clone(),
            other => vec![other.clone()],
        });
    strings
        .filter_map(|s| s.as_str().map(<[u8]>::to_vec).ok())
        .map(|bytes| bytes.chunks(2).filter(|pair| pair == &[0, 0]).count())
        .sum()
}

/// A test font's `@font-face` source as a `data:` URL.
fn test_font(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/css-properties/assets")
        .join(name);
    let bytes = std::fs::read(path).expect("the test font is in the repository");
    format!("url(data:font/ttf;base64,{})", base64(&bytes))
}

/// A bundled font's `@font-face` source as a `data:` URL.
fn bundled_font(name: &str) -> String {
    format!("url(data:font/ttf;base64,{})", base64(&font_file(name)))
}

/// A cut-off table cell in the family `F`, defined by `faces`, with `overflow` as its
/// `text-overflow`.
fn cut_off_cell(faces: &str, weight: u16, overflow: &str) -> String {
    format!(
        "<style>{faces} td {{ font: {weight} 16px F, sans-serif; white-space: nowrap; \
         overflow: hidden; text-overflow: {overflow}; width: 60px; max-width: 60px }} \
         table {{ table-layout: fixed; width: 60px }}</style>\
         <table><tr><td>ABABABABABABABABABAB</td></tr></table>"
    )
}

#[track_caller]
fn assert_ends_with_drawn(body: &str, mark: &str) {
    let copied = copied_text(body);
    assert!(copied.ends_with(mark), "{copied:?}");
    assert!(copied.starts_with("AB"), "{copied:?}");
    assert_eq!(missing_glyphs(body), 0, "{copied:?}");
}

#[test]
fn ellipsis_in_a_font_that_has_it() {
    for (file, weight) in [
        ("NotoSansMono-Regular.ttf", 400),
        ("NotoSansMono-Bold.ttf", 700),
    ] {
        let face = format!(
            "@font-face {{ font-family: F; font-weight: {weight}; src: {} }}",
            bundled_font(file)
        );
        assert_ends_with_drawn(&cut_off_cell(&face, weight, "ellipsis"), "\u{2026}");
    }
}

#[test]
fn ellipsis_falls_back_from_a_font_that_lacks_it() {
    for weight in [400, 700] {
        let face = format!(
            "@font-face {{ font-family: F; font-weight: 100 900; src: {} }}",
            test_font("simple.ttf")
        );
        let body = cut_off_cell(&face, weight, "ellipsis");
        assert_ends_with_drawn(&body, "\u{2026}");
        assert_ends_with_drawn(&cut_off_cell(&face, weight, "\"\u{bb}\""), "\u{bb}");
    }
}

/// A family split by `unicode-range`, as web fonts are served: the later face, which
/// comes first, lacks the ellipsis; the earlier one has it.
#[test]
fn ellipsis_takes_the_face_whose_range_has_it() {
    for weight in [400, 700] {
        let faces = format!(
            "@font-face {{ font-family: F; font-weight: 100 900; src: {}; \
             unicode-range: U+0000-00FF, U+2000-206F }} \
             @font-face {{ font-family: F; font-weight: 100 900; src: {}; \
             unicode-range: U+0100-02FF }}",
            bundled_font(if weight == 700 {
                "NotoSansMono-Bold.ttf"
            } else {
                "NotoSansMono-Regular.ttf"
            }),
            test_font("simple.ttf"),
        );
        let body = cut_off_cell(&faces, weight, "ellipsis");
        assert_ends_with_drawn(&body, "\u{2026}");
        let fonts = embedded_fonts(&body);
        assert!(
            fonts.iter().all(|f| f.contains("NotoSansMono")),
            "{fonts:?}"
        );
    }
}
