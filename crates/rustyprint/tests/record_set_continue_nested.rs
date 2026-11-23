//! Records that continue a table inside other boxes: a document cut at its tables into
//! the document up to the first rows of a table, pieces of rows, and the rest of the
//! document, prints as the document printed whole.

mod common;

use common::{assert_same_print, page_count, pages, print_pieces};
use rustyprint::{Error, RecordSet, RenderOptions, render};

/// A part of a document to cut at its tables.
enum Part {
    Html(&'static str),
    /// A table, with the names of the elements it is in below `<body>`, outermost first,
    /// what it has before its rows, and its rows.
    Table {
        within: &'static [&'static str],
        top: &'static str,
        rows: Vec<String>,
    },
}

fn row(index: usize) -> String {
    let words = "lorem ipsum dolor ".repeat(index * 5 % 7);
    format!(
        "<tr><td>Row {index}</td><td>Item {} {words}</td><td>{}.50</td></tr>",
        index * 7 % 1000,
        index * 13 % 500
    )
}

fn rows(count: usize) -> Vec<String> {
    (0..count).map(row).collect()
}

/// A document of `parts`, whole, and cut into pieces with `size` rows of each table at
/// the most, as a caller cuts it: the document up to and including the first rows of a
/// table, then `<table data-rp-continue>` pieces of rows, then the rest of the document
/// after the table, which starts with an element with `data-rp-continue` for each element
/// the table is in, and goes on with the HTML that follows the table's `</table>`.
fn cut(style: &str, parts: &[Part], size: usize) -> (String, Vec<String>) {
    let head = format!("<style>{style}</style>");
    let mut whole = head.clone();
    let mut piece = head.clone();
    let mut pieces = Vec::new();
    for part in parts {
        match part {
            Part::Html(html) => {
                whole.push_str(html);
                piece.push_str(html);
            }
            Part::Table { within, top, rows } => {
                whole.push_str(&format!(
                    "<table>{top}<tbody>{}</tbody></table>",
                    rows.concat()
                ));
                // The piece is cut off after the first rows: the table and the elements
                // around it are left open.
                let (first, rest) = rows.split_at(size.min(rows.len()));
                piece.push_str(&format!("<table>{top}<tbody>{}", first.concat()));
                pieces.push(std::mem::replace(&mut piece, head.clone()));
                for chunk in rest.chunks(size) {
                    pieces.push(format!(
                        "{head}<table data-rp-continue><tbody>{}</tbody></table>",
                        chunk.concat()
                    ));
                }
                for name in *within {
                    piece.push_str(&format!("<{name} data-rp-continue>"));
                }
            }
        }
    }
    pieces.push(piece);
    (whole, pieces)
}

fn options() -> RenderOptions {
    RenderOptions {
        stylesheet: Some("@page { @bottom-center { content: 'Page ' counter(page) } }".into()),
        ..RenderOptions::default()
    }
}

/// Fails unless the document of `parts` prints in pieces of `size` rows as it prints
/// whole, on more pages than two, and returns the print.
fn assert_cut_prints_as_whole(style: &str, parts: &[Part], size: usize) -> Vec<u8> {
    let (whole, pieces) = cut(style, parts, size);
    let whole = render(&whole, &options()).unwrap().pdf;
    assert!(page_count(&whole) > 2, "the table takes pages");
    let printed = print_pieces(&pieces, &options());
    assert_same_print(&printed, &whole);
    printed
}

const CELLS: &str = "table { width: 100%; border-collapse: collapse }
    td, th { border: 1px solid #444; padding: 3px 6px }
    tbody tr:nth-child(even) { background: #eef }";

const HEAD: &str = "<thead><tr><th>Number</th><th>What it is</th><th>Amount</th></tr></thead>";

#[test]
fn a_table_in_a_box_in_pieces_prints_as_the_whole_document() {
    let style = format!("{CELLS} .box {{ border: 3px solid #a00; padding: 8px; margin: 10px }}");
    let parts = [
        Part::Html("<h1>Report</h1><div class='box'><h2>The rows</h2>"),
        Part::Table {
            within: &["div"],
            top: HEAD,
            rows: rows(200),
        },
        Part::Html("</div>"),
    ];
    assert_cut_prints_as_whole(&style, &parts, 40);
}

#[test]
fn a_table_four_boxes_deep_in_pieces_prints_as_the_whole_document() {
    // The boxes have borders, padding and backgrounds, which are painted on every page
    // they are on, as they are in the whole document: sliced, and cloned for one. The
    // table is laid out automatically in a box of a percentage width.
    let style = format!(
        "{CELLS}
         table {{ width: auto }}
         .page {{ border: 2px solid #333; padding: 6px; background: #f8f8f0 }}
         .widget {{ border: 4px double #06c; padding: 10px 4px; margin: 12px 0 }}
         .box {{ width: 80%; border: 1px dashed #090; padding: 5px;
                 box-decoration-break: clone; background: #efe }}
         .content {{ padding: 0 7px 9px; border-left: 6px solid #fc0 }}"
    );
    let parts = [
        Part::Html(
            "<div class='page'><div class='widget'><h2>Widget</h2><div class='box'>\
             <div class='content'><p>Before the table</p>",
        ),
        Part::Table {
            within: &["div", "div", "div", "div"],
            top: HEAD,
            rows: rows(230),
        },
        Part::Html("</div></div></div></div>"),
    ];
    for size in [40, 7] {
        assert_cut_prints_as_whole(&style, &parts, size);
    }
}

#[test]
fn what_follows_a_table_in_its_boxes_prints_as_the_whole_document() {
    // The rest of the document goes on in the box of the table, after its boxes, in other
    // widgets and in a footer.
    let style = format!(
        "{CELLS} .widget {{ border: 2px solid #06c; padding: 6px; margin: 8px 0 }}
         .content {{ padding: 4px; background: #fafafa }}"
    );
    let parts = [
        Part::Html("<div class='widget'><div class='content'>"),
        Part::Table {
            within: &["div", "div"],
            top: HEAD,
            rows: rows(150),
        },
        Part::Html(
            "<p>Note under the table</p></div><p>Under the content</p></div>\
             <div class='widget'><h2>Another widget</h2><p>Its text</p></div>\
             <footer><p>End of the report</p></footer>",
        ),
    ];
    let printed = assert_cut_prints_as_whole(&style, &parts, 30);
    let text: String = pages(&printed).into_iter().map(|(text, _)| text).collect();
    for said in [
        "Note under the table",
        "Another widget",
        "End of the report",
    ] {
        assert_eq!(text.matches(said).count(), 1, "{said}");
    }
}

#[test]
fn the_header_of_a_table_in_boxes_repeats_on_every_page() {
    let style = format!("{CELLS} .box {{ border: 1px solid #000; padding: 4px }}");
    let parts = [
        Part::Html("<div class='box'><div class='box'>"),
        Part::Table {
            within: &["div", "div"],
            top: "<thead><tr><th>Number</th><th>Heading of the column</th><th>Amount</th>\
                  </tr></thead><tfoot><tr><td colspan=3>Carried over</td></tr></tfoot>",
            rows: rows(200),
        },
        Part::Html("</div></div>"),
    ];
    let printed = assert_cut_prints_as_whole(&style, &parts, 25);
    for (index, (text, _)) in pages(&printed).iter().enumerate() {
        let page = index + 1;
        assert_eq!(
            text.matches("Heading of the column").count(),
            1,
            "page {page}"
        );
        assert_eq!(text.matches("Carried over").count(), 1, "page {page}");
    }
}

#[test]
fn a_second_table_later_in_the_document_is_continued_too() {
    let style = format!(
        "{CELLS} .widget {{ border: 2px solid #06c; padding: 6px; margin: 8px 0 }}
         .grid {{ border: 1px solid #999 }}"
    );
    let parts = [
        Part::Html("<div class='widget'><h2>First</h2>"),
        Part::Table {
            within: &["div"],
            top: HEAD,
            rows: rows(120),
        },
        Part::Html(
            "</div><p>Between the widgets</p><div class='widget'><h2>Second</h2>\
             <div class='grid'>",
        ),
        Part::Table {
            within: &["div", "div"],
            top: HEAD,
            rows: rows(140),
        },
        Part::Html("</div></div><p>After both</p><p>And the end of the document</p>"),
    ];
    assert_cut_prints_as_whole(&style, &parts, 35);
}

/// Adds the first piece of `parts`, then the next, which has rows for its table, and
/// returns what adding the rows did.
fn add_rows_to(style: &str, parts: &[Part]) -> Result<rustyprint::Added, Error> {
    let (_, pieces) = cut(style, parts, 20);
    let mut set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    set.add(&pieces[0]).unwrap();
    set.add_continuing(&pieces[1])
}

fn in_box(open: &'static str, within: &'static [&'static str], close: &'static str) -> [Part; 3] {
    [
        Part::Html(open),
        Part::Table {
            within,
            top: HEAD,
            rows: rows(60),
        },
        Part::Html(close),
    ]
}

#[test]
fn a_table_in_a_flex_or_table_cell_box_cannot_be_continued() {
    // The size of such a box depends on the rows in it, which a piece does not all have.
    let refused = [
        (
            in_box(
                "<div style='display: flex'><div>",
                &["div", "div"],
                "</div></div>",
            ),
            "display: flex",
        ),
        (
            in_box(
                "<div><div style='display: table-cell'>",
                &["div", "div"],
                "</div></div>",
            ),
            "display: table-cell",
        ),
        (
            in_box("<div style='width: max-content'>", &["div"], "</div>"),
            "width of its content",
        ),
        (
            in_box("<div style='float: left'>", &["div"], "</div>"),
            "a float",
        ),
    ];
    for (parts, reason) in refused {
        let failed = add_rows_to(CELLS, &parts).unwrap_err();
        let Error::RecordSet(message) = &failed else {
            panic!("{failed:?}");
        };
        assert!(
            message.contains("data-rp-continue") && message.contains(reason),
            "{message}"
        );
    }
    // A box that lays its content out as a block does is fine.
    let block = in_box("<div style='display: flow-root'>", &["div"], "</div>");
    assert_eq!(
        add_rows_to(CELLS, &block).unwrap().warnings,
        Vec::<String>::new()
    );
}

#[test]
fn a_box_to_continue_that_the_flow_has_not_open_is_one_of_its_own_with_a_warning() {
    let mut set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    set.add("<div><p>In the box</p></div>").unwrap();
    // The same name goes on in the box that is open.
    let added = set
        .add_continuing("<div data-rp-continue><p>More</p></div>")
        .unwrap();
    assert_eq!(added.warnings, Vec::<String>::new());
    let added = set
        .add_continuing("<section data-rp-continue><p>Not a div</p></section>")
        .unwrap();
    assert_eq!(added.warnings.len(), 1, "{:?}", added.warnings);
    assert!(
        added.warnings[0].contains("<section>"),
        "{:?}",
        added.warnings
    );
    let (pdf, _) = set.finish().unwrap();
    let whole = render(
        "<div><p>In the box</p><p>More</p></div><section><p>Not a div</p></section>",
        &RenderOptions::default(),
    )
    .unwrap();
    assert_same_print(&pdf, &whole.pdf);
}
