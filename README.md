# rustyprint

rustyprint turns HTML and CSS into PDF. It is written in Rust with a layout engine of its own, and needs no browser, no external runtime and no network access.

- **HTML:** the elements of the HTML5 specification, rendered as static, paginated PDF.
- **CSS:** the properties, selectors, at-rules and values of the MDN CSS reference that apply to HTML, including flexbox, grid, multi-column, tables, floats, positioning, paged media (`@page`, margin boxes, running headers, page counters), web fonts, variable and color fonts, transforms, filters and masks.
- **Coverage:** of the 1,080 elements and CSS features tracked, 1,049 are complete and the other 31 work in part, with their gaps listed.
- **Deterministic:** the same input always gives a byte-identical PDF.
- **Built for untrusted input:** nothing is fetched from the network unless you allow the host, and every render runs within limits on size, page count and time.

The trackers ([html-elements.md](html-elements.md) and the `css-*.md` files) list the status of every element and CSS feature, with notes on what is missing, and [progress.html](progress.html) summarises them. [plan.md](plan.md) is the plan and records which milestone the work is on.

| Use it as | Get it |
| --- | --- |
| [A command-line tool](#use-the-command-line) | An archive from the [Releases page](https://github.com/vryan95/rustyprint/releases), or `cargo install` |
| [An HTTP service](#run-the-http-service) | `docker run -p 8080:8080 ghcr.io/vryan95/rustyprint-server` |
| [A Ruby gem](#use-it-from-ruby) | `gem install rustyprint` |
| [A NuGet package](#use-it-from-net) | `dotnet add package Rustyprint` |
| [An npm package](#use-it-from-nodejs) | `npm install rustyprint` |
| [A Rust library](#use-it-from-rust) | A Git dependency |
| [A C library](#use-it-from-c) | An archive from the [Releases page](https://github.com/vryan95/rustyprint/releases) |

## Install

Each release on the [Releases page](https://github.com/vryan95/rustyprint/releases) has the program ready to run, for Linux, macOS and Windows on x64 and arm64:

| Platform | Archive |
| --- | --- |
| Linux (glibc 2.17 or newer) | `rustyprint-<version>-x86_64-unknown-linux-gnu.tar.gz`, `…-aarch64-unknown-linux-gnu.tar.gz` |
| Linux, static (Alpine and other musl distributions) | `rustyprint-<version>-x86_64-unknown-linux-musl.tar.gz`, `…-aarch64-unknown-linux-musl.tar.gz` |
| macOS | `rustyprint-<version>-aarch64-apple-darwin.tar.gz` (Apple silicon), `…-x86_64-apple-darwin.tar.gz` (Intel) |
| Windows | `rustyprint-<version>-x86_64-pc-windows-msvc.zip`, `…-aarch64-pc-windows-msvc.zip` |

Each archive holds the program, a single file with the fonts it needs built in, with the README and the licenses. `SHA256SUMS` lists the checksums of every archive.

Or build it from source, with Rust 1.98 or newer ([rustup.rs](https://rustup.rs)):

```sh
git clone https://github.com/vryan95/rustyprint.git
cd rustyprint
cargo build --release
```

The program is `target/release/rustyprint` (`rustyprint.exe` on Windows), a single file with the fonts it needs built in. Copy it anywhere on your `PATH`, or install it into Cargo's bin directory with:

```sh
cargo install --path crates/rustyprint-cli
```

## Use the command line

Render a file:

```sh
rustyprint invoice.html -o invoice.pdf
```

Images, stylesheets and fonts the HTML refers to by relative or root-relative URL are read from the HTML file's directory: `img/logo.png` and `/img/logo.png` are both `img/logo.png` there.

Choose the page and margins, which follow the CSS `margin` shorthand:

```sh
rustyprint report.html -o report.pdf --page-size Letter --landscape --margin "15mm 20mm"
```

Read the HTML from standard input and write the PDF to standard output with `-`:

```sh
cat page.html | rustyprint - -o - > page.pdf
```

HTML from standard input has no directory, so it reads no local files unless you give it some:

```sh
generate-invoice | rustyprint - -o invoice.pdf --assets ./templates --asset logo.png=./brand/logo-2026.png
```

`--asset NAME=FILE` serves a file under the name the HTML uses, such as `<img src="logo.png">`, and can be repeated.

### Options

| Option | What it does |
| --- | --- |
| `-o, --output FILE` | The PDF to write, or `-` for standard output. Required. |
| `--page-size SIZE` | `A3`, `A4`, `A5`, `Letter` or `Legal`. Default `A4`. A document's own `@page { size }` rule takes precedence. |
| `--landscape` | Turns the page so it is wider than it is tall. |
| `--margin MARGIN` | One to four lengths, as in CSS: `2cm`, `15mm 20mm`, `0.5in 1in 0.5in 1in`. Units: `px`, `mm`, `cm`, `in`, `pt`. Default `2cm`. |
| `--title TITLE` | The PDF's title. Defaults to the document's `<title>`. |
| `--css FILE` | A stylesheet applied after the document's own. |
| `--assets DIR` | The directory relative and root-relative URLs resolve against. Defaults to the input file's directory. |
| `--asset NAME=FILE` | A file the document refers to as `NAME`. Repeatable. |
| `--allow-host HOST` | A host that absolute `https` URLs may be fetched from, such as `cdn.example.com` or `*.example.com`. Repeatable. Redirects must stay on allowed hosts. |
| `--allow-http` | Also fetch plain `http` URLs from the allowed hosts. |
| `--options FILE` | Options as JSON (below). The other flags override what it sets. |
| `--fail-on-warnings` | Fail, without writing the PDF, if there are any warnings. |

Run `rustyprint --help` for the full list.

### Options as JSON

`--options` takes the same JSON options the HTTP service and the Ruby, .NET and Node.js packages take. [schema/render-options.schema.json](schema/render-options.schema.json) describes every field. All fields are optional:

```json
{
  "page_size": "A4",
  "landscape": false,
  "margin": "15mm",
  "title": "Invoice 42",
  "stylesheet": "body { font-family: sans-serif }",
  "remote": { "allowed_hosts": ["cdn.example.com"] },
  "limits": { "max_pages": 50, "timeout_ms": 10000 }
}
```

`page_size` also takes an exact size, such as `{ "width": "100mm", "height": "150mm" }`.

### Remote images and stylesheets

By default rustyprint never touches the network: absolute `http` and `https` URLs are skipped with a warning. Allow each host to fetch from, including the hosts that redirects lead to:

```sh
rustyprint invoice.html -o invoice.pdf \
  --allow-host app.example.com \
  --allow-host storage.example.net
```

Each fetch is limited to 10 seconds and 10 MB, with at most 5 redirects and 100 fetches per document. The `remote` JSON option changes these limits.

### Limits

Every render runs within limits, so a hostile or broken document cannot run forever or exhaust memory. Change them with the `limits` JSON option.

| Limit | Default | JSON field |
| --- | --- | --- |
| HTML size | 20 MB | `max_input_bytes` |
| DOM nodes | 500,000 | `max_nodes` |
| Element nesting depth | 512 | `max_depth` |
| Pages | 2,000 | `max_pages` |
| Pages of a whole record set | 200,000 | `max_total_pages` |
| Image size (larger images are skipped with a warning) | 100 megapixels | `max_image_pixels` |
| Pixels drawn for `<svg>` elements and SVG image files that cannot be drawn as shapes, in all, for a document or a whole record set (one that needs more is left blank with a warning) | 64 megapixels | `max_svg_pixels` |
| Time | 60 seconds | `timeout_ms` (`null` for no limit) |

### Warnings and exit codes

Problems that do not stop the render, such as unknown CSS or an image that cannot be loaded, are printed to standard error as warnings, and the PDF is still written. With `--fail-on-warnings` they fail the run instead.

| Exit code | Meaning |
| --- | --- |
| 0 | The PDF was written |
| 1 | A file could not be read or written |
| 2 | The arguments or options are invalid |
| 3 | The document exceeds a limit |
| 4 | Rendering took longer than the deadline |
| 5 | There were warnings, with `--fail-on-warnings` |
| 70 | An internal error in rustyprint |

## Run the HTTP service

`rustyprint-server` serves the same renderer over HTTP. Each release publishes it as a container image for `linux/amd64` and `linux/arm64`:

```sh
docker run -p 8080:8080 ghcr.io/vryan95/rustyprint-server:latest
```

Send the HTML as JSON, with the same options as `--options` takes, or as `multipart/form-data` with the files it refers to:

```sh
curl -H 'Content-Type: application/json' \
  -d '{"html": "<h1>Hello</h1>", "options": {"page_size": "A4", "margin": "15mm"}}' \
  -o hello.pdf http://localhost:8080/v1/pdf

curl -F index.html=@invoice.html -F img/logo.png=@brand/logo.png -o invoice.pdf http://localhost:8080/v1/pdf
```

The response is the PDF, with the warnings in its headers. [crates/rustyprint-server/README.md](crates/rustyprint-server/README.md) is the API reference: both request formats, the response headers, the errors, the metrics, and the environment variables that configure the server and set what requests may do.

## Use it from Ruby

The `rustyprint` gem, for Ruby 3.2 and newer, with the native library built into a gem for each platform. In Rails it adds `render pdf:` to controllers.

```ruby
output = Rustyprint.render(html, page_size: "A4", margin: "15mm",
                                 assets: { "logo.png" => Pathname("app/assets/images/logo.png") })
File.binwrite("invoice.pdf", output.pdf)
```

Install it with `gem install rustyprint`, or add `gem "rustyprint"` to your Gemfile. [bindings/ruby/README.md](bindings/ruby/README.md) describes the options, errors and Rails support, and [demos/rails](demos/rails) is a Rails app that uses it.

## Use it from .NET

The `Rustyprint` NuGet package, for .NET 8 and newer, with the native library for every platform inside it.

```csharp
var options = new RenderOptions { PageSize = PageSize.A4, Margin = "15mm" };
options.Assets.AddFile("logo.png", "wwwroot/logo.png");
RenderOutput output = await PdfRenderer.RenderAsync(html, options, cancellationToken);
File.WriteAllBytes("invoice.pdf", output.Pdf);
```

Install it with `dotnet add package Rustyprint`. [bindings/dotnet/README.md](bindings/dotnet/README.md) describes the options, errors and use in ASP.NET Core, and [demos/dotnet](demos/dotnet) is an ASP.NET Core app that uses it.

## Use it from Node.js

The `rustyprint` npm package, for Node.js 22 and newer, with the native library in a package for each platform that npm installs along with it. Renders run on worker threads, so the event loop keeps running.

```js
const fs = require('node:fs')
const { render } = require('rustyprint')

const { pdf, warnings } = await render(html, {
  pageSize: 'A4',
  margin: '15mm',
  assets: { 'logo.png': fs.readFileSync('public/logo.png') }
})
fs.writeFileSync('invoice.pdf', pdf)
```

Install it with `npm install rustyprint`. [bindings/node/README.md](bindings/node/README.md) describes the options, errors, threads and platforms, and [demos/node](demos/node) is an Express app that uses it.

## Use it from Rust

The `rustyprint` crate is the engine itself. It is not published on crates.io, so depend on it through Git, at a release's tag:

```toml
[dependencies]
rustyprint = { git = "https://github.com/vryan95/rustyprint", tag = "v1.2.0", features = ["serde"] }
```

The `serde` feature adds `RenderOptions::from_json`, for options as JSON; without it, build `RenderOptions` in code.

```rust
use rustyprint::{RenderOptions, render};

let options = RenderOptions::from_json(r#"{"page_size": "Letter", "margin": "1in"}"#)?; // `serde` feature
let output = render("<h1>Hello</h1>", &options)?;
std::fs::write("hello.pdf", &output.pdf)?;
for warning in &output.warnings {
    eprintln!("warning: {warning}");
}
```

`render` is safe to call from several threads at once. Each render runs on a thread of its own, so deeply nested documents cannot overflow the caller's stack, and a panic inside it comes back as an error.

### Many records as one PDF

`RecordSet` prints many documents one after another as one PDF file, such as a statement per customer, with one of them in memory at a time. Each `add` lays one record out, writes its pages to the writer and lets go of the record.

```rust
use rustyprint::{RecordSet, RenderOptions};

let file = std::io::BufWriter::new(std::fs::File::create("statements.pdf")?);
let mut set = RecordSet::open(file, &RenderOptions::default())?;
for name in ["Ann", "Ben"] {
    let added = set.add(&format!("<h1>Statement for {name}</h1>"))?;
    println!("{} pages", added.pages);
}
let (_file, summary) = set.finish()?;
```

- Every record is a document of its own with the options of the set, and starts on a new page, unless it is added with `add_continuing`. Its first page is a `:first` page.
- Page numbers run through the set: `counter(page)` carries on from the records before, and so do left and right pages. `counter(pages)` is the number of pages of the record, not of the set.
- Links reach inside a record only: a link to an element of another record does not resolve.
- Fonts are embedded once, and an image is written once however many records use it. Decoded images are kept between records only from the second record that uses the same file, up to 64 MB of pixels, so a record's own photo is freed with the record.
- The limits hold for each record. The whole set has `Limits::max_total_pages` pages at the most, 200,000 by default.
- A record that fails ends the set: every later call returns an error, and what the writer has is not a whole PDF file.

#### A long document a piece at a time

`add_continuing` adds a piece of the record before it, instead of a document of its own: its content is laid out as if it were at the end of that record's `<body>`, on the page where that record ended. A long table fed a few hundred rows at a time prints as it does printed whole, with about one piece in memory.

```rust
let mut set = RecordSet::open(file, &RenderOptions::default())?;
set.add("<h1>Cover</h1>")?;                                   // a record of its own
set.add(&format!("{style}<table>{head}<tbody>{rows_1}</tbody></table>"))?;
set.add_continuing(&format!("{style}<table data-rp-continue>{head}<tbody>{rows_2}</tbody></table>"))?;
set.add_continuing("<p>Under the table, on its last page.</p>")?;
set.add("<h1>Appendix</h1>")?;                                // starts a new page
```

- **Open elements.** `<body>` is open, and so is the last child of an open element when nothing but white space and comments follows it: the elements that the HTML of the flow so far leaves unclosed. The *open table* is the first `<table>` among them, at any depth; a table in one of its cells is not.
- **The table rule.** A piece that starts with `<table data-rp-continue>` gives the rows of that table's `<tbody>` elements to the last `<tbody>` of the open table. The rows go straight under the rows before them, with no gap and no header in the middle of a page. The open table and the elements it is in have to be blocks among blocks, whose sizes do not depend on the rows: `display: block`, `flow-root` or `list-item`, not floated or absolutely positioned, no `width` of `min-content`, `max-content`, `fit-content` or `calc-size()`, and not in a flex, grid, table or multi-column box. Otherwise the `add` fails and names the element in the way. The `<colgroup>`, `<thead>`, `<tfoot>` and `<caption>` of the piece's table are not used: the table keeps those of its first piece, and its header and footer repeat on every page, as in a table printed whole. A table without the attribute is a table of its own. With the attribute and no open table, it is a table of its own as well, and the `add` gives a warning.
- **The rest of the document.** An element with `data-rp-continue` at the start of a piece's `<body>` stands for the open child of `<body>` of the same name: its content is added to that element's. One at the start of that content stands for the open element below, and so on; its attributes are not used, and one with no open element of its name at its place is an element of its own, with a warning. So a document cut at its biggest table prints as it does whole, in three kinds of piece:

  ```rust
  // <body><div class="page"><div class="widget"><table>…rows…</table><p>Note</p></div>
  // <div class="widget">Totals</div></div><footer>End</footer></body>
  set.add(&format!("{style}<div class=\"page\"><div class=\"widget\"><table>{head}<tbody>{first_rows}"))?;
  for rows in later_rows { set.add_continuing(&format!("{style}<table data-rp-continue><tbody>{rows}</tbody></table>"))?; }
  set.add_continuing(&format!("{style}<div data-rp-continue><div data-rp-continue><p>Note</p></div>\
      <div class=\"widget\">Totals</div></div><footer>End</footer>"))?;
  ```

  The first piece is the document up to its first rows, left open; the last is one start tag with `data-rp-continue` for each element the table is in, then the HTML after `</table>` as it is.
- **Column widths** are those the table had when its first piece was laid out, at any depth. The boxes around it have the widths of the whole document, since they do not depend on the rows. For `table-layout: fixed`, and for an automatic table whose widths its first rows or its `<col>` elements decide, those are the widths of the table printed whole. Wider content in a later piece wraps in the columns as they are. Rows with more columns than the table started with fail the `add`, before anything of them is written.
- **What carries on over the pieces:** collapsed borders, `:nth-child()`, `:nth-of-type()`, `:first-child`, sibling combinators, counters that rows add to with `counter-increment`, page numbers, left and right pages, and `counter(pages)` in a page margin, which is the number of pages of the whole set. `:nth-child(… of S)` counts the rows that are already written as matching `S`; `:nth-last-child()` and `:last-child` see the rows there are so far; a `reversed()` counter does not count the rows still to come.
- **Styles** are those of the record that started: a `<style>` or stylesheet link in the `<head>` of a piece that continues is not used, with a warning unless the first record had the same one.
- **The open page.** The last page of a record is a page of the file as soon as the record is added, and what it paints is written when the page is full, when a record that does not continue is added, or when the set is finished. It is the page the record's last element starts on: pages after it that hold only the ends of the open elements, such as their bottom borders, are laid out again with the next piece and written when the flow ends. Until then the set holds what that page paints and the part of the document that is on it or after it. `Added::pages` of a piece that continues is the pages it started: none when it fits on the open page. A gradient on the open page is written again each time the page is laid out again.
- **Kept for the whole flow**, rather than let go of when their page is written: elements with `position: fixed` or `position: running()`, `counter-reset` or `counter-set`, `display: list-item`, and `<style>` and `<link>` elements, or elements that have one inside. A `position: fixed` box repeats on the pages laid out with its own piece, not on those of later pieces. Links and `target-counter()` reach only what the flow still holds. A table's `height`, and rows that span into rows of a later piece, are not carried over.

## Use it from C

`rustyprint-ffi` is rustyprint as a shared library with a C ABI, for C and any language that can call C; the Ruby gem and the NuGet and npm packages are built on it. Each release has it for every platform as `rustyprint-native-<version>-<target>.tar.gz` (`.zip` for Windows), with the header, or build it with `cargo build --release -p rustyprint-ffi`.

```c
rp_request *request = rp_request_new(options_json, strlen(options_json));
rp_request_add_asset(request, "img/logo.png", logo, logo_len);
rp_result *result = NULL;
if (rp_render(request, html, strlen(html), &result) == RP_STATUS_OK) {
    size_t len;
    const uint8_t *pdf = rp_result_pdf(result, &len);
    /* ... */
}
rp_result_free(result);
rp_request_free(request);
```

The options are the same JSON as `--options` takes. [crates/rustyprint-ffi/README.md](crates/rustyprint-ffi/README.md) describes the whole API and how to link it.

## Performance

rustyprint 1.0.4 compared with [WeasyPrint](https://weasyprint.org) 70.0, a widely used HTML-to-PDF converter, rendering the same documents with the same Noto fonts. Each run starts a new process, as a command-line conversion does, so start-up time counts. Times are medians; memory is the most the process was seen to use, sampled every 5 ms.

| Document | Pages | rustyprint | WeasyPrint | |
| --- | --- | --- | --- | --- |
| Invoice ([demos/samples/invoice](demos/samples/invoice/index.html)) | 3 | 33 ms, 21 MB | 1.03 s, 75 MB | 31× faster |
| One-page report ([bench/report-one.html](bench/report-one.html)) | 1 | 17 ms, 18 MB | 0.93 s, 74 MB | 53× faster |
| Long report ([bench/report-long.html](bench/report-long.html)) | 498 / 500 | 1.6 s, 195 MB | 38.2 s, 628 MB | 25× faster |

Used as a library, without starting a process, rendering takes:

| Document | Pages | Time |
| --- | --- | --- |
| Letter | 1 | 4 ms |
| Report | 21 | 38 ms |
| Invoice with a 200-row table | 6 | 30 ms |
| Book of 3,000 paragraphs | 250 | 0.78 s |

Measured on an AMD Ryzen 7 5700U laptop in October 2026: the comparison on Linux in Docker on WSL 2, the library times on Linux on WSL 2. To run the comparison yourself, with WeasyPrint and the Python package `psutil` installed:

```sh
python bench/run.py target/release/rustyprint weasyprint bench-out
```

and the library benchmarks with `cargo bench -p rustyprint --bench render`. CI runs them every night and compares each run with the night before.

## Fonts

rustyprint bundles Noto Sans, Noto Serif and Noto Sans Mono, under the SIL Open Font License ([fonts/OFL.txt](fonts/OFL.txt)): `serif`, the initial font, is Noto Serif. For characters these lack, it falls back to Noto Naskh Arabic (Noto Sans Arabic for sans-serif text) and Noto Sans SC, so Arabic and Chinese text renders without any `@font-face`. Han characters in Japanese or Korean text also use the Simplified Chinese glyphs. Emoji, which none of these cover, fall back to Noto Color Emoji and are drawn in color. Noto Sans SC adds 7.8 MB and Noto Color Emoji 5 MB; building with `--no-default-features` leaves both out, or turn off just the `cjk` or `emoji` feature. Other fonts come from `@font-face` rules, in TrueType, OpenType, WOFF or WOFF2. System fonts are not used, so a document renders the same on every machine.

## Development

The crates in [crates/](crates/):

| Crate | What it does |
| --- | --- |
| `rustyprint-dom` | Parses HTML into a DOM |
| `rustyprint-css` | Parses CSS and computes styles: selectors, the cascade, values |
| `rustyprint-layout` | Box tree, layout, text, pagination and painting |
| `rustyprint-pdf` | Writes the PDF |
| `rustyprint` | The public API: `render(html, options)` |
| `rustyprint-cli` | The `rustyprint` command |
| `rustyprint-server` | The HTTP service |
| `rustyprint-ffi` | The native library: a C ABI over `rustyprint` |
| `rustyprint-tests` | The fixture suite |

`master` is protected: changes arrive through pull requests, which merge once CI passes. CI runs these on every pull request, so run them before you push:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all --check
cargo deny check
```

and, for the packages, `bundle exec rake` in [bindings/ruby](bindings/ruby) (the tests and the documentation check), `dotnet test` in [bindings/dotnet](bindings/dotnet), and `npm test` and `npm run check-types` in [bindings/node](bindings/node).

The fixture suite has one HTML file in [tests/fixtures/](tests/fixtures/) for every row of the trackers, with its expectations inside it. After changing a tracker or a milestone in the plan, regenerate the progress page with `cargo run -p rustyprint-tests --bin progress`. [CLAUDE.md](CLAUDE.md) has more about how the work is organised.

## License

rustyprint is licensed under either of

- the Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)), or
- the MIT license ([LICENSE-MIT](LICENSE-MIT)),

at your option. Unless you say otherwise, any contribution you submit for inclusion in rustyprint is licensed as above, without any additional terms or conditions.

The bundled Noto fonts are under the SIL Open Font License ([fonts/OFL.txt](fonts/OFL.txt)). Every package and archive carries the three licenses.
