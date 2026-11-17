use rustyprint_layout::display::Document;
use rustyprint_layout::{Color, DisplayItem, Font, Page, Point, Size, TextRun};

fn page(lines: &[&str]) -> Page {
    let items = lines
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let origin = Point::new(96.0, 96.0 + 24.0 * i as f32);
            DisplayItem::Text(TextRun::new(
                Font::bundled(),
                16.0,
                Color::BLACK,
                origin,
                *text,
            ))
        })
        .collect();
    Page::new(Size::A4, items)
}

fn document(pages: Vec<Page>) -> Document {
    Document {
        title: Some("Test".to_owned()),
        pages,
        bookmarks: Vec::new(),
        color_profiles: Vec::new(),
        device_cmyk: None,
    }
}

fn parse(pdf: &[u8]) -> lopdf::Document {
    lopdf::Document::load_mem(pdf).expect("output parses as PDF")
}

#[test]
fn writes_one_page_per_page() {
    let pdf =
        rustyprint_pdf::write(&document(vec![page(&["one"]), page(&["two"]), page(&[])])).unwrap();
    assert_eq!(parse(&pdf).get_pages().len(), 3);
}

#[test]
fn page_size_is_in_points() {
    let pdf = rustyprint_pdf::write(&document(vec![page(&[])])).unwrap();
    let doc = parse(&pdf);
    let page_id = doc.get_pages()[&1];
    let media_box = doc
        .get_dictionary(page_id)
        .unwrap()
        .get(b"MediaBox")
        .unwrap()
        .as_array()
        .unwrap();
    let size: Vec<f32> = media_box.iter().map(|v| v.as_float().unwrap()).collect();
    assert_eq!(size[0..2], [0.0, 0.0]);
    assert!((size[2] - 595.28).abs() < 0.01, "width {}", size[2]);
    assert!((size[3] - 841.89).abs() < 0.01, "height {}", size[3]);
}

#[test]
fn text_is_extractable() {
    let pdf = rustyprint_pdf::write(&document(vec![page(&["Hello office", "Ελληνικά"])])).unwrap();
    let text = parse(&pdf).extract_text(&[1]).unwrap();
    assert!(text.contains("Hello office"), "extracted {text:?}");
    assert!(text.contains("Ελληνικά"), "extracted {text:?}");
}

#[test]
fn fonts_are_subset_and_shared() {
    let pdf = rustyprint_pdf::write(&document(vec![page(&["Hello"]), page(&["World"])])).unwrap();
    // The full font is about 600 KB.
    assert!(pdf.len() < 20_000, "PDF is {} bytes", pdf.len());
    let doc = parse(&pdf);
    let fonts = doc.objects.values().filter(|object| {
        object.as_dict().is_ok_and(|dict| {
            dict.get(b"Subtype")
                .is_ok_and(|s| s.as_name().is_ok_and(|n| n == b"Type0"))
        })
    });
    assert_eq!(fonts.count(), 1);
}

#[test]
fn output_is_deterministic() {
    let doc = document(vec![page(&["Same input,", "same bytes."])]);
    assert_eq!(
        rustyprint_pdf::write(&doc).unwrap(),
        rustyprint_pdf::write(&doc).unwrap()
    );
}

#[test]
fn native_colors_keep_their_color_space() {
    use rustyprint_layout::Rect;
    use rustyprint_layout::display::{ColorProfile, DEVICE_CMYK, NativeColor};
    let native = |space: u32, components: [u16; 4]| Color {
        native: Some(NativeColor { space, components }),
        ..Color::rgb(0, 0, 0)
    };
    let items = vec![
        DisplayItem::Rect {
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            color: native(0, [0, 65535, 0, 0]),
        },
        DisplayItem::Rect {
            rect: Rect::new(10.0, 0.0, 10.0, 10.0),
            color: native(DEVICE_CMYK, [0, 0, 0, 65535]),
        },
        DisplayItem::Rect {
            rect: Rect::new(20.0, 0.0, 10.0, 10.0),
            color: Color::rgb(255, 0, 0),
        },
    ];
    let mut document = document(vec![Page::new(Size::A4, items)]);
    let profile = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/css-at-rules/assets/cmyk.icc"),
    )
    .unwrap();
    document.color_profiles = vec![ColorProfile {
        data: profile.into(),
        components: 4,
    }];
    let pdf = parse(&rustyprint_pdf::write(&document).unwrap());
    let (_, page) = pdf.get_pages().into_iter().next().unwrap();
    let content = String::from_utf8_lossy(&pdf.get_page_content(page)).into_owned();
    assert!(content.contains("/Cs0 cs"), "{content}");
    assert!(content.contains("0 1 0 0 scn"), "{content}");
    assert!(content.contains("0 0 0 1 k"), "{content}");
    assert!(content.contains("1 0 0 rg"), "{content}");
    // The profile is embedded as an ICC-based color space with four components.
    let icc = pdf
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .find(|stream| stream.dict.get(b"N").is_ok())
        .expect("an ICC profile stream");
    assert_eq!(icc.dict.get(b"N").unwrap().as_i64().unwrap(), 4);
}

/// The media box and the text of each page, in page order.
fn pages_of(pdf: &[u8]) -> Vec<(Vec<f32>, String)> {
    let doc = parse(pdf);
    doc.get_pages()
        .into_iter()
        .map(|(number, id)| {
            let media_box = doc
                .get_dictionary(id)
                .unwrap()
                .get(b"MediaBox")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_float().unwrap())
                .collect();
            (media_box, doc.extract_text(&[number]).unwrap())
        })
        .collect()
}

/// Writes each of `batches` with a call of its own.
fn write_in_batches(batches: Vec<Vec<Page>>) -> Vec<u8> {
    let setup = document(Vec::new());
    let mut session =
        rustyprint_pdf::Session::new(Vec::new(), rustyprint_pdf::Setup::of(&setup)).unwrap();
    let mut count = 0;
    for pages in batches {
        count += pages.len();
        session.add_pages(&document(pages)).unwrap();
        assert_eq!(session.page_count(), count);
    }
    session.finish().unwrap()
}

/// A page that shows `image`.
fn image_page(image: &std::sync::Arc<rustyprint_layout::Image>) -> Page {
    let item = DisplayItem::Image {
        image: image.clone(),
        rect: rustyprint_layout::Rect::new(10.0, 10.0, 20.0, 20.0),
        smooth: true,
        orientation: 1,
        alpha: 255,
    };
    Page::new(Size::A4, vec![item])
}

fn image() -> std::sync::Arc<rustyprint_layout::Image> {
    std::sync::Arc::new(rustyprint_layout::Image {
        width: 2,
        height: 2,
        data: rustyprint_layout::ImageData::Pixels {
            samples: vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
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
fn pages_added_in_batches_are_the_pages_of_one_document() {
    let small = Size {
        width: 400.0,
        height: 300.0,
    };
    let pages = vec![
        page(&["first page"]),
        Page::new(small, page(&["second, small"]).items),
        page(&["third page", "Ελληνικά"]),
        page(&[]),
        page(&["fifth page"]),
    ];
    let at_once = rustyprint_pdf::write(&document(pages.clone())).unwrap();
    let batched = write_in_batches(vec![
        pages[..2].to_vec(),
        pages[2..3].to_vec(),
        pages[3..].to_vec(),
    ]);
    let expected = pages_of(&at_once);
    assert_eq!(expected.len(), 5);
    assert!(expected[1].1.contains("second, small"), "{expected:?}");
    assert_eq!(expected[1].0, [0.0, 0.0, 300.0, 225.0]);
    assert_eq!(pages_of(&batched), expected);
}

#[test]
fn an_image_on_pages_of_two_batches_is_written_once() {
    let image = image();
    let setup = document(Vec::new());
    let mut session =
        rustyprint_pdf::Session::new(Vec::new(), rustyprint_pdf::Setup::of(&setup)).unwrap();
    session
        .add_pages(&document(vec![image_page(&image)]))
        .unwrap();
    // Once its page is written, the session no longer holds the image's pixels.
    assert_eq!(std::sync::Arc::strong_count(&image), 1);
    session
        .add_pages(&document(vec![page(&["no image"]), image_page(&image)]))
        .unwrap();
    // The same picture decoded again is the same image, wherever it is in memory.
    drop(image);
    session
        .add_pages(&document(vec![image_page(&self::image())]))
        .unwrap();
    let doc = parse(&session.finish().unwrap());
    let images: Vec<lopdf::ObjectId> = doc
        .objects
        .iter()
        .filter(|(_, object)| {
            object.as_stream().is_ok_and(|stream| {
                stream
                    .dict
                    .get(b"Subtype")
                    .is_ok_and(|s| s.as_name().is_ok_and(|n| n == b"Image"))
            })
        })
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(images.len(), 1);
    // The pages of both batches draw that one object.
    let pages = doc.get_pages();
    for number in [1, 3, 4] {
        let (resources, _) = doc.get_page_resources(pages[&number]).unwrap();
        let x_objects = resources.unwrap().get(b"XObject").unwrap();
        let x_objects = x_objects.as_dict().unwrap();
        let drawn: Vec<lopdf::ObjectId> = x_objects
            .iter()
            .map(|(_, object)| object.as_reference().unwrap())
            .collect();
        assert_eq!(drawn, images, "page {number}");
    }
}

#[test]
fn a_batch_is_written_when_it_is_added() {
    struct Counting(std::rc::Rc<std::cell::Cell<usize>>);
    impl std::io::Write for Counting {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.set(self.0.get() + bytes.len());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let written = std::rc::Rc::new(std::cell::Cell::new(0));
    let setup = document(Vec::new());
    let mut session =
        rustyprint_pdf::Session::new(Counting(written.clone()), rustyprint_pdf::Setup::of(&setup))
            .unwrap();
    let before = written.get();
    session
        .add_pages(&document(vec![image_page(&image()), page(&["text"])]))
        .unwrap();
    let after_first = written.get();
    // Two pages, their content streams and the image are more than a few hundred bytes.
    assert!(after_first > before + 300, "{before} then {after_first}");
    session.add_pages(&document(vec![page(&["more"])])).unwrap();
    assert!(written.get() > after_first);
    session.finish().unwrap();
}

#[test]
fn a_writer_that_fails_is_an_error() {
    struct Failing;
    impl std::io::Write for Failing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let setup = document(Vec::new());
    let result = rustyprint_pdf::Session::new(Failing, rustyprint_pdf::Setup::of(&setup));
    assert!(matches!(result, Err(rustyprint_pdf::Error::Io(_))));
}

#[test]
fn the_cross_reference_table_points_at_every_object() {
    use rustyprint_layout::display::{Bookmark, Destination};
    let image = image();
    let mut first = document(vec![image_page(&image), page(&["one"])]);
    first.bookmarks = vec![Bookmark {
        level: 1,
        title: "One".to_owned(),
        target: Destination { page: 1, y: 0.0 },
    }];
    let setup = document(Vec::new());
    let mut session =
        rustyprint_pdf::Session::new(Vec::new(), rustyprint_pdf::Setup::of(&setup)).unwrap();
    session.add_pages(&first).unwrap();
    session
        .add_pages(&document(vec![page(&["two"]), image_page(&image)]))
        .unwrap();
    let pdf = session.finish().unwrap();
    assert_eq!(parse(&pdf).get_pages().len(), 4);

    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    let tail = text(&pdf[pdf.len() - 40..]);
    let start: usize = tail
        .split("startxref\n")
        .nth(1)
        .and_then(|rest| rest.strip_suffix("\n%%EOF"))
        .expect("the file ends with the table's offset")
        .parse()
        .unwrap();
    let table = &pdf[start..];
    let header_end = 5 + table[5..].iter().position(|&b| b == b'\n').unwrap() + 1;
    let header = text(&table[..header_end]);
    let count: usize = header
        .strip_prefix("xref\n0 ")
        .expect("one section from object zero")
        .trim_end()
        .parse()
        .unwrap();
    assert!(count > 10, "{count} objects");
    for number in 1..count {
        let entry = text(&table[header_end + 20 * number..header_end + 20 * (number + 1)]);
        assert!(
            entry.ends_with(" 00000 n\r\n"),
            "object {number}: {entry:?}"
        );
        let offset: usize = entry[..10].parse().unwrap();
        let object = format!("{number} 0 obj\n");
        assert!(
            pdf[offset..].starts_with(object.as_bytes()),
            "object {number} is not at {offset}"
        );
    }
}

#[test]
fn links_and_bookmarks_of_a_batch_point_to_its_pages() {
    use rustyprint_layout::display::{Bookmark, Destination, Link, LinkTarget};
    let mut linking = page(&["to the next page"]);
    linking.links = vec![Link {
        rect: rustyprint_layout::Rect::new(96.0, 80.0, 100.0, 20.0),
        target: LinkTarget::Internal(Destination { page: 1, y: 0.0 }),
    }];
    let mut second = document(vec![linking, page(&["target"])]);
    second.bookmarks = vec![Bookmark {
        level: 1,
        title: "Target".to_owned(),
        target: Destination { page: 1, y: 0.0 },
    }];
    let setup = document(Vec::new());
    let mut session =
        rustyprint_pdf::Session::new(Vec::new(), rustyprint_pdf::Setup::of(&setup)).unwrap();
    session
        .add_pages(&document(vec![page(&["a"]), page(&["b"])]))
        .unwrap();
    session.add_pages(&second).unwrap();
    let doc = parse(&session.finish().unwrap());
    let pages = doc.get_pages();

    let annotations = doc
        .get_dictionary(pages[&3])
        .unwrap()
        .get(b"Annots")
        .unwrap();
    let annotation = annotations.as_array().unwrap()[0].as_reference().unwrap();
    let action = doc.get_dictionary(annotation).unwrap().get(b"A").unwrap();
    let destination = action.as_dict().unwrap().get(b"D").unwrap();
    let linked = destination.as_array().unwrap()[0].as_reference().unwrap();
    assert_eq!(linked, pages[&4]);

    let bookmark = doc
        .objects
        .values()
        .filter_map(|object| object.as_dict().ok())
        .find(|dict| dict.has(b"Title") && dict.has(b"Dest"))
        .expect("an outline item");
    let destination = bookmark.get(b"Dest").unwrap().as_array().unwrap();
    assert_eq!(destination[0].as_reference().unwrap(), pages[&4]);
}

#[test]
fn images_that_differ_are_written_apiece() {
    let other = {
        let mut other = std::sync::Arc::into_inner(image()).unwrap();
        other.data = rustyprint_layout::ImageData::Pixels {
            samples: vec![7; 12],
            gray: false,
            alpha: None,
        };
        std::sync::Arc::new(other)
    };
    let pdf = write_in_batches(vec![vec![image_page(&image())], vec![image_page(&other)]]);
    let images = parse(&pdf)
        .objects
        .values()
        .filter_map(|object| object.as_stream().ok())
        .filter(|stream| stream.dict.has(b"Width"))
        .count();
    assert_eq!(images, 2);
}

#[test]
fn a_writer_emptied_after_every_batch_gets_the_whole_file() {
    let batches = || {
        vec![
            vec![image_page(&image()), page(&["one"])],
            vec![page(&["two"])],
            vec![page(&["three"]), image_page(&image())],
        ]
    };
    let whole = write_in_batches(batches());

    let setup = document(Vec::new());
    let mut session =
        rustyprint_pdf::Session::new(Vec::new(), rustyprint_pdf::Setup::of(&setup)).unwrap();
    let mut drained = Vec::new();
    for pages in batches() {
        session.add_pages(&document(pages)).unwrap();
        let taken = std::mem::take(session.get_mut());
        assert!(!taken.is_empty());
        drained.extend(taken);
    }
    drained.extend(session.finish().unwrap());
    assert_eq!(drained, whole);
    assert_eq!(parse(&drained).get_pages().len(), 5);
}
