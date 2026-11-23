//! A long table inside boxes printed a piece at a time holds about one piece, as a table
//! that is a child of `<body>` does. This test has a test binary to itself, so that no
//! other test's memory is counted.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustyprint::{RecordSet, RenderOptions, render};

/// The most the process may grow while it prints a table in pieces.
const MAX_GROWTH_KB: usize = 48 * 1024;

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

const STYLE: &str = "<style>
    .page { border: 2px solid #333; padding: 6px }
    .widget { border: 3px double #06c; padding: 8px; margin: 10px 0; background: #fcfcf8 }
    .box { border: 1px solid #090; padding: 4px }
    .content { padding: 0 6px; border-left: 4px solid #fc0 }
    table { width: 100%; border-collapse: collapse }
    td, th { border: 1px solid #888; padding: 2px 6px }
    tbody tr:nth-child(even) { background: #eef }
</style>";

const START: &str = "<div class='page'><div class='widget'><h2>Deliveries</h2>\
    <div class='box'><div class='content'>";

const END: &str = "</div></div></div><div class='widget'><p>Totals</p></div></div>\
    <footer><p>End of the report</p></footer>";

fn rows(rows: std::ops::Range<usize>) -> String {
    rows.map(|index| {
        format!(
            "<tr><td>{index}</td><td>Customer number {}</td><td>Order {}</td>\
             <td>{}.00</td><td>Delivered to depot {}</td></tr>",
            index * 31 % 977,
            index * 17 % 4999,
            index * 7 % 1000,
            index % 12
        )
    })
    .collect()
}

const HEAD: &str = "<thead><tr><th>Line</th><th>Customer</th><th>Order</th><th>Amount</th>\
    <th>Delivery</th></tr></thead>";

/// The document of `count` rows, whole.
fn whole(count: usize) -> String {
    format!(
        "{STYLE}{START}<table>{HEAD}<tbody>{}</tbody></table>{END}",
        rows(0..count)
    )
}

fn options() -> RenderOptions {
    RenderOptions {
        stylesheet: Some(
            "@page { @bottom-right { content: 'Page ' counter(page) ' of ' counter(pages) } }"
                .to_owned(),
        ),
        ..RenderOptions::default()
    }
}

fn mean(times: &[Duration]) -> Duration {
    times.iter().sum::<Duration>() / times.len() as u32
}

/// Peak growth of the process while `work` runs, read every 20 ms, and how long it took.
fn growth_while<T>(work: impl FnOnce() -> T) -> (T, usize, Duration) {
    let done = AtomicBool::new(false);
    let start = resident_kb();
    let started = Instant::now();
    let (result, peak) = std::thread::scope(|scope| {
        let watch = scope.spawn(|| {
            let mut peak = 0;
            while !done.load(Ordering::Relaxed) {
                peak = peak.max(resident_kb());
                std::thread::sleep(Duration::from_millis(20));
            }
            peak
        });
        let result = work();
        done.store(true, Ordering::Relaxed);
        (result, watch.join().unwrap())
    });
    (result, peak.saturating_sub(start), started.elapsed())
}

/// What printing `count` rows in pieces of `piece` did: how much the process grew after
/// the first piece, the pages, and how long a piece took at the start and at the end.
struct InPieces {
    growth_kb: usize,
    pages: usize,
    first: Duration,
    last: Duration,
}

fn print_in_pieces(count: usize, piece: usize) -> InPieces {
    let mut set = RecordSet::open(std::io::sink(), &options()).unwrap();
    // The first piece loads the fonts, which stay for the whole set. It ends after the
    // first rows, with the table and its boxes open.
    let first = format!("{STYLE}{START}<table>{HEAD}<tbody>{}", rows(0..piece));
    set.add(&first).unwrap();
    let ((first, last), growth_kb, _) = growth_while(|| {
        let mut times = Vec::new();
        for start in (piece..count).step_by(piece) {
            let rows = rows(start..(start + piece).min(count));
            let html = format!("{STYLE}<table data-rp-continue><tbody>{rows}</tbody></table>");
            let started = Instant::now();
            let added = set.add_continuing(&html).unwrap();
            times.push(started.elapsed());
            assert_eq!(added.warnings, Vec::<String>::new());
        }
        let rest = format!("{STYLE}{}{END}", "<div data-rp-continue>".repeat(4));
        assert_eq!(
            set.add_continuing(&rest).unwrap().warnings,
            Vec::<String>::new()
        );
        (mean(&times[..10]), mean(&times[times.len() - 10..]))
    });
    let (_, summary) = set.finish().unwrap();
    InPieces {
        growth_kb,
        pages: summary.pages,
        first,
        last,
    }
}

#[test]
fn a_long_table_in_boxes_in_pieces_holds_one_piece_and_keeps_its_pace() {
    let (rows, piece) = (20_000, 200);
    let printed = print_in_pieces(rows, piece);
    let (first, last) = (printed.first, printed.last);
    assert!(
        last < first * 3,
        "the last pieces took {last:?} each, the first ones {first:?}"
    );
    // A fifth of the document printed whole takes more memory than all of it in pieces.
    let whole = whole(rows / 5);
    let (pdf, whole_growth, whole_time) = growth_while(|| render(&whole, &options()));
    pdf.unwrap();
    let growth = printed.growth_kb;
    println!(
        "{rows} rows in boxes on {} pages in pieces of {piece}: the process grew by \
         {growth} KB; a piece took {first:?} at the start and {last:?} at the end. {} rows \
         whole: the process grew by {whole_growth} KB in {whole_time:?}",
        printed.pages,
        rows / 5
    );
    assert!(growth < MAX_GROWTH_KB, "the process grew by {growth} KB");
    assert!(
        growth < whole_growth,
        "{growth} KB in pieces, {whole_growth} KB whole"
    );
}

/// The numbers for 200,000 rows in pieces of 500, against the same document printed whole.
/// `cargo test --release -p rustyprint --test record_set_continue_nested_memory --
/// --ignored --nocapture --test-threads 1`; `WHOLE_ROWS` sets the rows of the document
/// printed whole, 200,000 if it is not set.
#[test]
#[ignore = "prints 200,000 rows, and the document whole, which takes minutes"]
fn two_hundred_thousand_rows_in_boxes_for_the_numbers() {
    let (rows, piece) = (200_000, 500);
    render("<p>The fonts</p>", &options()).unwrap();
    let printed = print_in_pieces(rows, piece);
    println!(
        "{rows} rows in boxes on {} pages in pieces of {piece}: the process grew by {} KB; \
         a piece took {:?} at the start and {:?} at the end",
        printed.pages, printed.growth_kb, printed.first, printed.last
    );
    assert!(
        printed.growth_kb < MAX_GROWTH_KB,
        "the process grew by {} KB",
        printed.growth_kb
    );
    let whole_rows = std::env::var("WHOLE_ROWS")
        .ok()
        .and_then(|rows| rows.parse().ok())
        .unwrap_or(rows);
    let whole = whole(whole_rows);
    let (pdf, growth, time) = growth_while(|| render(&whole, &options()));
    let bytes = pdf.map(|output| output.pdf.len());
    println!("{whole_rows} rows whole: the process grew by {growth} KB in {time:?}: {bytes:?}");
}
