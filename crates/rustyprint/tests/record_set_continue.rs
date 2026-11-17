//! Records that continue: a document printed a piece at a time is the document printed
//! whole.

mod common;

use std::ops::Range;

use common::{page_count, paragraphs};
use lopdf::Object;
use lopdf::content::Content;
use rustyprint::{Error, RecordSet, RenderOptions, render};

/// What a page paints: every operation of its content, in order.
type Painted = Vec<(String, Vec<Object>)>;

/// The text and the content of each page of a PDF file.
fn pages(pdf: &[u8]) -> Vec<(String, Painted)> {
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

fn number(object: &Object) -> Option<f32> {
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
fn same(a: &Object, b: &Object) -> bool {
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
fn assert_same_print(pieces: &[u8], whole: &[u8]) {
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

/// A table to print whole and in pieces.
struct Shape {
    style: &'static str,
    /// What the table has before its rows: `<colgroup>`, `<thead>`, `<tfoot>`.
    top: &'static [&'static str],
    row: fn(usize) -> String,
}

fn plain_row(index: usize) -> String {
    format!(
        "<tr><td>Row {index}</td><td>Item {}</td><td>{}.50</td></tr>",
        index * 7 % 1000,
        index * 13 % 500
    )
}

/// Rows of one, two or several lines.
fn uneven_row(index: usize) -> String {
    let words = "lorem ipsum dolor sit amet ".repeat(index * 5 % 11);
    format!(
        "<tr><td>Row {index}</td><td>{words}</td><td>{}</td></tr>",
        index % 9
    )
}

impl Shape {
    fn table(&self, rows: Range<usize>, continues: bool) -> String {
        let attribute = if continues { " data-rp-continue" } else { "" };
        let rows: String = rows.map(self.row).collect();
        format!(
            "<table{attribute}>{}<tbody>{rows}</tbody></table>",
            self.top.concat()
        )
    }

    fn head(&self) -> String {
        format!("<style>{}</style>", self.style)
    }

    fn whole(&self, rows: usize) -> String {
        format!("{}{}", self.head(), self.table(0..rows, false))
    }

    /// The table of `rows` rows in pieces of `size` rows.
    fn pieces(&self, rows: usize, size: usize) -> Vec<String> {
        (0..rows)
            .step_by(size)
            .map(|start| {
                let piece = self.table(start..(start + size).min(rows), start > 0);
                format!("{}{piece}", self.head())
            })
            .collect()
    }
}

/// Prints `pieces` as one flow: the first as a record, the others continuing it.
fn print_pieces(pieces: &[String], options: &RenderOptions) -> Vec<u8> {
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

/// Fails unless `rows` rows of `shape` print in pieces of `size` as they print whole, on
/// more pages than one.
fn assert_pieces_print_as_whole(shape: &Shape, rows: usize, size: usize) {
    let options = RenderOptions {
        stylesheet: Some("@page { @bottom-center { content: 'Page ' counter(page) } }".into()),
        ..RenderOptions::default()
    };
    let whole = render(&shape.whole(rows), &options).unwrap().pdf;
    assert!(page_count(&whole) > 2, "the table takes pages");
    let pieces = print_pieces(&shape.pieces(rows, size), &options);
    assert_same_print(&pieces, &whole);
}

const COLUMNS: &str = "<colgroup><col style='width: 20%'><col style='width: 50%'><col></colgroup>";
const HEAD: &str = "<thead><tr><th>Number</th><th>What it is</th><th>Amount</th></tr></thead>";
const FOOT: &str = "<tfoot><tr><td colspan=3>Carried over</td></tr></tfoot>";

const FIXED: Shape = Shape {
    style: "table { table-layout: fixed; width: 100%; border-spacing: 2px }
            td, th { border: 1px solid #444; padding: 3px }",
    top: &[COLUMNS, HEAD],
    row: plain_row,
};

#[test]
fn a_fixed_layout_table_in_pieces_prints_as_the_whole_table() {
    assert_pieces_print_as_whole(&FIXED, 230, 40);
}

#[test]
fn an_auto_layout_table_in_pieces_prints_as_the_whole_table() {
    // The header is wider than any cell under it, so the first rows decide the columns.
    let shape = Shape {
        style: "td, th { border: 1px solid #444; padding: 3px 12px }",
        top: &[
            "<thead><tr><th>Number of the row</th><th>What the row is about</th>\
               <th>Amount in full</th></tr></thead>",
        ],
        row: plain_row,
    };
    assert_pieces_print_as_whole(&shape, 230, 40);
}

#[test]
fn a_header_and_a_footer_repeat_on_the_pages_of_a_table_in_pieces() {
    let shape = Shape {
        style: FIXED.style,
        top: &[COLUMNS, HEAD, FOOT],
        row: plain_row,
    };
    assert_pieces_print_as_whole(&shape, 230, 40);
    let pieces = print_pieces(&shape.pieces(230, 40), &RenderOptions::default());
    for (index, (text, _)) in pages(&pieces).iter().enumerate() {
        assert_eq!(text.matches("What it is").count(), 1, "page {}", index + 1);
        assert_eq!(
            text.matches("Carried over").count(),
            1,
            "page {}",
            index + 1
        );
    }
}

#[test]
fn collapsed_borders_carry_on_over_the_pieces_of_a_table() {
    // Every fifth row has a thick border under it, which wins over the thin border of the
    // row below: a row at the start of a piece has the border of the row before it.
    let shape = Shape {
        style: "table { border-collapse: collapse; table-layout: fixed; width: 100% }
                td, th { border: 1px solid #444; padding: 3px }
                tbody tr:nth-child(5n) td { border-bottom: 4px solid #a00 }",
        top: &[HEAD],
        row: plain_row,
    };
    assert_pieces_print_as_whole(&shape, 230, 40);
    assert_pieces_print_as_whole(&shape, 230, 5);
}

#[test]
fn rows_of_differing_heights_break_where_they_do_in_the_whole_table() {
    let shape = Shape {
        style: FIXED.style,
        top: FIXED.top,
        row: uneven_row,
    };
    assert_pieces_print_as_whole(&shape, 180, 25);
    assert_pieces_print_as_whole(&shape, 180, 33);
}

#[test]
fn a_piece_that_ends_where_a_page_ends_prints_as_the_whole_table() {
    let options = RenderOptions::default();
    let whole = render(&FIXED.whole(230), &options).unwrap().pdf;
    // The rows of the first page, and of the first two.
    let on_page = |page: usize| pages(&whole)[page].0.matches("Row ").count();
    let first = on_page(0);
    let two = first + on_page(1);
    assert!(first > 10 && two > first, "{first} and {two} rows");
    for size in [first, two] {
        assert_pieces_print_as_whole(&FIXED, 230, size);
    }
}

#[test]
fn a_table_a_row_at_a_time_prints_as_the_whole_table() {
    let shape = Shape {
        style: FIXED.style,
        top: FIXED.top,
        row: uneven_row,
    };
    assert_pieces_print_as_whole(&shape, 90, 1);
}

#[test]
fn stripes_and_counters_carry_on_over_the_pieces_of_a_table() {
    let shape = Shape {
        style: "table { table-layout: fixed; width: 100%; border-spacing: 0; counter-reset: line }
                td { padding: 3px }
                tbody tr { counter-increment: line }
                tbody tr:nth-child(even) { background: #dde }
                tbody tr:nth-of-type(3n + 1) td { color: #a00 }
                tbody tr:first-child td { font-weight: bold }
                tbody tr + tr td { border-top: 1px solid #999 }
                td:first-child::before { content: counter(line) '. ' }",
        top: &[HEAD],
        row: plain_row,
    };
    // Pieces of an odd number of rows, so that a piece on its own would start its stripes
    // on the other color.
    assert_pieces_print_as_whole(&shape, 230, 37);
    let pieces = print_pieces(&shape.pieces(230, 37), &RenderOptions::default());
    let text: String = pages(&pieces).into_iter().map(|(text, _)| text).collect();
    let text: String = text.split_whitespace().collect();
    assert!(
        text.ends_with("230.Row229Item603477.50"),
        "the last row is numbered 230"
    );
}

#[test]
fn paragraphs_continue_under_a_table_on_its_page() {
    let options = RenderOptions::default();
    let table = FIXED.whole(60);
    let after = format!("<h2>After the table</h2>{}", paragraphs(40));
    let whole = render(&format!("{table}{after}"), &options).unwrap().pdf;
    let pieces = print_pieces(&[table, after], &options);
    assert_same_print(&pieces, &whole);
    // The paragraphs start on the page the table ends on.
    let texts = pages(&pieces);
    let page = texts.iter().find(|(text, _)| text.contains("Row 59"));
    let page = &page.expect("the last row is on a page").0;
    assert!(page.contains("After the table"), "{page}");
}

#[test]
fn a_running_header_of_the_first_piece_is_on_the_pages_of_the_later_ones() {
    let options = RenderOptions {
        stylesheet: Some(
            ".running { position: running(head) }
             @page { @top-center { content: element(head) } }"
                .into(),
        ),
        ..RenderOptions::default()
    };
    let header = "<div class='running'>Stock report</div>";
    let whole = format!("{header}{}", FIXED.whole(230));
    let whole = render(&whole, &options).unwrap().pdf;
    let mut pieces = FIXED.pieces(230, 40);
    pieces[0].insert_str(0, header);
    let pieces = print_pieces(&pieces, &options);
    assert_same_print(&pieces, &whole);
    for (index, (text, _)) in pages(&pieces).iter().enumerate() {
        assert_eq!(
            text.matches("Stock report").count(),
            1,
            "page {}",
            index + 1
        );
    }
}

#[test]
fn paragraphs_in_pieces_print_as_the_whole_document() {
    let options = RenderOptions::default();
    let piece = |index: usize| format!("<h2>Part {index}</h2>{}", paragraphs(7));
    let pieces: Vec<String> = (0..12).map(piece).collect();
    let whole = render(&pieces.concat(), &options).unwrap().pdf;
    assert!(page_count(&whole) > 3);
    assert_same_print(&print_pieces(&pieces, &options), &whole);
}

#[test]
fn records_and_pieces_mix_in_one_set() {
    let options = RenderOptions {
        stylesheet: Some(
            "@page { @bottom-center { content: 'Page ' counter(page) ' of ' counter(pages) } }"
                .into(),
        ),
        ..RenderOptions::default()
    };
    let mut set = RecordSet::open(Vec::new(), &options).unwrap();
    let mut added = vec![set.add("<h1>Cover of the report</h1>").unwrap().pages];
    for (index, piece) in FIXED.pieces(150, 30).iter().enumerate() {
        let piece = if index == 0 {
            set.add(piece)
        } else {
            set.add_continuing(piece)
        };
        added.push(piece.unwrap().pages);
        assert_eq!(set.pages(), added.iter().sum::<usize>());
    }
    added.push(set.add("<h1>Appendix of the report</h1>").unwrap().pages);
    let (pdf, summary) = set.finish().unwrap();
    let texts: Vec<String> = pages(&pdf).into_iter().map(|(text, _)| text).collect();
    let total = texts.len();
    assert_eq!(summary.pages, total);
    assert_eq!(added.iter().sum::<usize>(), total);

    // The table is on the pages it takes printed alone, between the cover and the appendix.
    let table = render(&FIXED.whole(150), &options).unwrap().pdf;
    let table: Vec<String> = pages(&table).into_iter().map(|(text, _)| text).collect();
    assert_eq!(total, table.len() + 2);
    assert!(texts[0].contains("Cover of the report") && !texts[0].contains("Row"));
    assert!(
        texts[total - 1].contains("Appendix of the report") && !texts[total - 1].contains("Row")
    );
    let rows = |text: &str| -> Vec<String> {
        let words = text.split_whitespace().map(str::to_owned);
        words.take_while(|word| word != "Page").collect()
    };
    for (index, alone) in table.iter().enumerate() {
        assert_eq!(
            rows(&texts[index + 1]),
            rows(alone),
            "page {} of the table",
            index + 1
        );
    }
    // Every page tells the pages of the whole set.
    for (index, text) in texts.iter().enumerate() {
        let squeezed: String = text.split_whitespace().collect();
        let folio = format!("Page{}of{total}", index + 1);
        assert!(squeezed.contains(&folio), "page {}: {text}", index + 1);
    }
}

#[test]
fn a_table_keeps_the_columns_of_its_first_piece() {
    // A later piece has a cell wider than the column: it wraps, and the columns stay.
    let options = RenderOptions::default();
    let shape = Shape {
        style: "td, th { border: 1px solid #444; padding: 3px 12px }",
        top: &["<thead><tr><th>Number of the row</th><th>What</th><th>Amount</th></tr></thead>"],
        row: plain_row,
    };
    let mut pieces = shape.pieces(80, 40);
    let wide = "<table data-rp-continue><tbody><tr><td>Row 80</td>\
                <td>a cell with a good many more words in it than the column is wide</td>\
                <td>1</td></tr></tbody></table>";
    pieces.push(wide.to_owned());
    let pdf = print_pieces(&pieces, &options);
    let narrow = print_pieces(&shape.pieces(80, 40), &options);
    let left_edges = |pdf: &[u8]| -> Vec<i32> {
        let mut edges: Vec<i32> = pages(pdf)[0]
            .1
            .iter()
            .filter(|(operator, _)| operator == "Tm")
            .filter_map(|(_, operands)| number(&operands[4]))
            .map(|x| x.round() as i32)
            .collect();
        edges.sort_unstable();
        edges.dedup();
        edges
    };
    assert_eq!(left_edges(&pdf), left_edges(&narrow));
}

#[test]
fn rows_with_more_columns_than_their_table_fail_the_record() {
    let mut set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    set.add(&FIXED.whole(10)).unwrap();
    let wider = "<table data-rp-continue><tbody><tr><td>1</td><td>2</td><td>3</td><td>4</td>\
                 </tr></tbody></table>";
    let failed = set.add_continuing(wider).unwrap_err();
    let Error::RecordSet(message) = &failed else {
        panic!("{failed:?}");
    };
    assert!(message.contains("3 columns"), "{message}");
    assert!(matches!(set.add("<p>next</p>"), Err(Error::RecordSet(_))));
}

#[test]
fn a_table_to_continue_without_one_before_it_is_a_table_of_its_own_with_a_warning() {
    let mut set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    set.add("<p>No table here</p>").unwrap();
    let table = "<table data-rp-continue><tbody><tr><td>Lone row</td></tr></tbody></table>";
    let added = set.add_continuing(table).unwrap();
    assert_eq!(added.pages, 0);
    assert_eq!(added.warnings.len(), 1, "{:?}", added.warnings);
    assert!(
        added.warnings[0].contains("data-rp-continue"),
        "{:?}",
        added.warnings
    );
    // A table without the attribute is a table of its own, and says nothing.
    let second = "<table><tbody><tr><td>Second table</td></tr></tbody></table>";
    assert_eq!(
        set.add_continuing(second).unwrap().warnings,
        Vec::<String>::new()
    );
    let (pdf, _) = set.finish().unwrap();
    let text = &pages(&pdf)[0].0;
    assert!(
        text.contains("Lone row") && text.contains("Second table"),
        "{text}"
    );
    assert_eq!(page_count(&pdf), 1);
}

#[test]
fn other_styles_in_a_piece_that_continues_are_not_used_with_a_warning() {
    let mut set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    set.add("<style>p { color: red }</style><p>One</p>")
        .unwrap();
    let same = set
        .add_continuing("<style>p { color: red }</style><p>Two</p>")
        .unwrap();
    assert_eq!(same.warnings, Vec::<String>::new());
    let other = set
        .add_continuing("<style>p { color: blue }</style><p>Three</p>")
        .unwrap();
    assert_eq!(other.warnings.len(), 1, "{:?}", other.warnings);
    assert!(
        other.warnings[0].contains("stylesheet"),
        "{:?}",
        other.warnings
    );
}

#[test]
fn the_first_record_of_a_set_can_be_added_as_one_that_continues() {
    let mut set = RecordSet::open(Vec::new(), &RenderOptions::default()).unwrap();
    assert_eq!(set.add_continuing("<p>First</p>").unwrap().pages, 1);
    assert_eq!(set.add_continuing("<p>Second</p>").unwrap().pages, 0);
    let (pdf, summary) = set.finish().unwrap();
    assert_eq!((summary.pages, summary.records), (1, 2));
    let whole = render("<p>First</p><p>Second</p>", &RenderOptions::default()).unwrap();
    assert_same_print(&pdf, &whole.pdf);
}
