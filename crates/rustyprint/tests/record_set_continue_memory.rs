//! A long table printed a piece at a time holds about one piece, and each piece takes as
//! long as the first ones did. This test has a test binary to itself, so that no other
//! test's memory is counted.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustyprint::{RecordSet, RenderOptions, render};

/// The rows of the table, and of each piece of it.
const ROWS: usize = 20_000;
const PIECE: usize = 200;

/// The rows of the table that is printed whole to compare: a fifth of the rows, since a
/// table takes longer to print whole the longer it is.
const WHOLE_ROWS: usize = ROWS / 5;

/// The most the process may grow while it prints the table in pieces.
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
    table { width: 100%; border-collapse: collapse }
    td, th { border: 1px solid #888; padding: 2px 6px }
    tbody tr:nth-child(even) { background: #eef }
</style>";

fn table(rows: std::ops::Range<usize>, continues: bool) -> String {
    let attribute = if continues { " data-rp-continue" } else { "" };
    let rows: String = rows
        .map(|index| {
            format!(
                "<tr><td>{index}</td><td>Customer number {}</td><td>Order {}</td>\
                 <td>{}.00</td><td>Delivered to depot {}</td></tr>",
                index * 31 % 977,
                index * 17 % 4999,
                index * 7 % 1000,
                index % 12
            )
        })
        .collect();
    format!(
        "{STYLE}<table{attribute}><thead><tr><th>Line</th><th>Customer</th><th>Order</th>\
         <th>Amount</th><th>Delivery</th></tr></thead><tbody>{rows}</tbody></table>"
    )
}

fn mean(times: &[Duration]) -> Duration {
    times.iter().sum::<Duration>() / times.len() as u32
}

/// How much the process grows while it prints a table of `rows` rows whole, with its
/// memory read while it prints, and how long that takes.
fn whole_growth_kb(rows: usize, options: &RenderOptions) -> (usize, Duration) {
    let whole = table(0..rows, false);
    let done = AtomicBool::new(false);
    let start = resident_kb();
    let started = Instant::now();
    let peak = std::thread::scope(|scope| {
        let watch = scope.spawn(|| {
            let mut peak = 0;
            while !done.load(Ordering::Relaxed) {
                peak = peak.max(resident_kb());
                std::thread::sleep(Duration::from_millis(20));
            }
            peak
        });
        let output = render(&whole, options);
        done.store(true, Ordering::Relaxed);
        output.unwrap();
        watch.join().unwrap()
    });
    (peak.saturating_sub(start), started.elapsed())
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

/// The whole table of [`ROWS`] rows, for the numbers: it takes minutes in a debug build.
/// `cargo test --release -p rustyprint --test record_set_continue_memory -- --ignored
/// --nocapture`
#[test]
#[ignore = "prints the whole table, which takes minutes in a debug build"]
fn the_whole_long_table_for_comparison() {
    render("<p>The fonts</p>", &options()).unwrap();
    let (growth, time) = whole_growth_kb(ROWS, &options());
    println!("{ROWS} rows whole: the process grew by {growth} KB in {time:?}");
}

#[test]
fn a_long_table_in_pieces_holds_one_piece_and_keeps_its_pace() {
    let options = options();
    let mut set = RecordSet::open(std::io::sink(), &options).unwrap();
    // The first piece loads the fonts, which stay for the whole set.
    set.add(&table(0..PIECE, false)).unwrap();
    let before = resident_kb();
    let mut peak = before;
    let mut times = Vec::new();
    for start in (PIECE..ROWS).step_by(PIECE) {
        let piece = table(start..start + PIECE, true);
        let started = Instant::now();
        let added = set.add_continuing(&piece).unwrap();
        times.push(started.elapsed());
        assert_eq!(added.warnings, Vec::<String>::new());
        assert!(added.pages > 0, "a piece of {PIECE} rows fills pages");
        peak = peak.max(resident_kb());
    }
    let growth = peak.saturating_sub(before);
    let (_, summary) = set.finish().unwrap();
    assert_eq!(summary.records, ROWS / PIECE);
    let pages = summary.pages;

    // A piece late in the table takes about as long as one at its start.
    let (first, last) = (mean(&times[..10]), mean(&times[times.len() - 10..]));
    assert!(
        last < first * 3,
        "the last pieces took {last:?} each, the first ones {first:?}"
    );

    // A fifth of the table printed whole takes more memory than all of it in pieces.
    let (whole_growth, whole_time) = whole_growth_kb(WHOLE_ROWS, &options);
    println!(
        "{ROWS} rows on {pages} pages in pieces of {PIECE}: the process grew by {growth} KB; \
         a piece took {first:?} at the start and {last:?} at the end. {WHOLE_ROWS} rows \
         whole: the process grew by {whole_growth} KB in {whole_time:?}"
    );
    assert!(
        growth < MAX_GROWTH_KB,
        "the process grew by {growth} KB while printing {ROWS} rows in pieces"
    );
    assert!(
        growth < whole_growth,
        "{ROWS} rows in pieces grew the process by {growth} KB, {WHOLE_ROWS} rows whole by \
         {whole_growth} KB"
    );
}
