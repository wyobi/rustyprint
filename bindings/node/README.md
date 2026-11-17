# rustyprint for Node.js

Renders HTML and CSS to PDF with [rustyprint](https://github.com/vryan95/rustyprint), a layout engine of its own written in Rust. It needs no browser, no external runtime and no network access, and the fonts it needs are built in.

```sh
npm install rustyprint
```

Node.js 22 or newer. It loads rustyprint's native library, the one the Ruby gem and the NuGet package use, through [koffi](https://koffi.dev).

## Render

```js
const fs = require('node:fs')
const { render } = require('rustyprint')

const { pdf, warnings } = await render(html, {
  pageSize: 'A4',
  margin: '15mm',
  title: 'Invoice 42',
  assets: {
    'logo.png': fs.readFileSync('logo.png'),
    'invoice.css': fs.readFileSync('app/pdf/invoice.css')
  }
})
fs.writeFileSync('invoice.pdf', pdf)
warnings // => ["invoice.css:12: unsupported property `colr`"]
```

`import { render } from 'rustyprint'` works the same from ES modules, and the package has TypeScript types.

`render` resolves to `pdf`, a Buffer, and `warnings`, problems found in the document's CSS and assets, as an array of strings. The HTML is a string, or UTF-8 bytes.

**Options** are the JSON options every rustyprint front end shares ([schema/render-options.schema.json](../../schema/render-options.schema.json)), with camelCase names: `pageSize`, `landscape`, `margin`, `title`, `stylesheet`, `remote` and `limits`. Names in snake_case, as in the schema, work too. An option left out keeps its default.

**Assets** are the files the document refers to by relative or root-relative URL, by the path it uses without a leading slash (`/css/app.css` is `css/app.css`): strings, Buffers, typed arrays or ArrayBuffers, in an object or a Map. Nothing else is read, and nothing is fetched from the network unless `remote.allowedHosts` allows the host.

## Fit a wide document to the page

`contentWidth` measures how wide a document's content is in a browser window of a given size, in CSS pixels: the window's width, or more when content reaches past its right edge. The document is laid out for the screen, on one continuous page. To fit the document on a page, render it with a `scale` of the page area's width divided by that width.

```js
const { contentWidth, render } = require('rustyprint')

const width = await contentWidth(html, { width: 1123, height: 794 }, { assets })
const area = 1123 - 2 * 20 // A4 landscape with 20px margins, in CSS pixels
const { pdf } = await render(html, { pageSize: 'A4', landscape: true, margin: '20px', scale: Math.min(1, area / width), assets })
```

`contentWidthSync` measures on the calling thread. Of the options, the stylesheet, the assets and the limits apply.

## Record sets: many documents as one PDF, streamed

A record set prints many documents, such as a statement per customer, as one PDF, one record at a time. Each `add` resolves to the bytes that record added to the file, and `finish` to the last bytes, so neither rustyprint nor your process ever holds the whole PDF.

```js
const { createRecordSet, toReadable } = require('rustyprint')

const set = createRecordSet({ pageSize: 'A4', assets: { 'logo.png': logo } })
const rows = db.query('SELECT * FROM statements').cursor()

// Pull mode: a record is read, printed and sent only when the response wants more.
const pdf = toReadable(set, async () => {
  const row = await rows.next()
  return row ? statementHtml(row) : null
})
response.setHeader('Content-Type', 'application/pdf')
pdf.pipe(response)
```

`toReadable(set, nextHtml)` is a Readable that calls `nextHtml` and prints its record only when the consumer asks for more bytes, and finishes the set when `nextHtml` gives null. A slow download therefore stops the records being read and printed, instead of the PDF piling up in memory. An error, or `destroy()`, aborts the set.

The set can be driven by hand as well:

```js
const { pdf, pages, warnings } = await set.add(html) // the bytes of this record's pages
const { pdf, pages, records } = await set.finish()   // the last bytes, and the totals
await set.abort()                                    // or give the set up
```

- Every record is a document of its own with the set's options and assets, and starts on a new page, unless it is added with `{ continue: true }`: see below. Page numbers run through the set, and `counter(pages)` is the number of pages of the whole set, so `"Page " counter(page) " of " counter(pages)` works across records.
- Fonts are embedded once, and an image used by many records is written once.
- `limits` hold for each record; `limits.maxTotalPages` (200,000 by default) bounds the set.
- One worker prints the whole set, from its first call until it is finished, aborted or fails, and renders nothing else meanwhile; the renderer's other workers keep rendering. `createRenderer({ concurrency })` has `createRecordSet` too.

### Records that continue: a long table in slices

`set.add(html, { continue: true })` adds a piece of the record before it instead of a document of its own: its content follows on the page where that record ended, as if it were at the end of that record's `<body>`. A document printed a piece at a time comes out as it does printed whole, and rustyprint holds about one piece.

```js
const set = createRecordSet({ stylesheet: '@page { @bottom-right { content: "Page " counter(page) " of " counter(pages) } }' })
const header = '<thead><tr><th>Line</th><th>Customer</th><th>Amount</th></tr></thead>'
let first = true
const pdf = toReadable(set, async () => {
  const rows = await cursor.nextRows(250)
  if (rows.length === 0) return null
  const table = `<table${first ? '' : ' data-rp-continue'}>${header}<tbody>${rows.map(rowHtml).join('')}</tbody></table>`
  const record = { html: `<style>${css}</style>${table}`, continue: !first }
  first = false
  return record
})
```

- **The table rule.** A piece that starts with `<table data-rp-continue>` gives the rows of its `<tbody>` to the table that the record before it ended with, which has to be the last thing in that record and a child of `<body>`. The rows go straight under the rows before them. The table keeps the `<colgroup>`, `<thead>`, `<tfoot>` and column widths of its first piece, and the header and footer repeat on every page, as in a table printed whole; those of a later piece are not used. A table without the attribute is a table of its own. With the attribute and no table to continue, it is a table of its own as well, with a warning. Rows with more columns than the table started with fail the `add`.
- **Column widths** are those of the first piece. For `table-layout: fixed`, and for an automatic table whose widths its first rows or its `<col>` elements decide, they are the widths of the table printed whole; wider content in a later piece wraps.
- **What carries on:** collapsed borders, `:nth-child()` and `:nth-of-type()` stripes, `:first-child`, sibling selectors, `counter-increment` counters, page numbers and `counter(pages)`.
- **Styles** are those of the record that started: a `<style>` in the `<head>` of a piece that continues is not used, with a warning unless it is one the first record had.
- **The open page.** What the last page of a record paints is written by the next call, or by `finish`, since a piece that continues may add to it: `add` resolves to the bytes written so far, and `pages` of a piece that continues is the pages it started, which is none when it fits on the open page.
- Records that start a page and records that continue mix in one set: a cover, a table in slices, an appendix.
- `toReadable` takes `{ html, continue: true }` from `nextHtml` for a record that continues.
- Calls on a set run one after another: a call made while another runs waits for it.
- A call that fails ends the set: the bytes so far are not a whole PDF, and later calls reject with a `RecordSetError`. Always `finish` or `abort` a set, so that its worker goes back to the others.

## Threads

`render` runs each render on a worker thread, so the event loop keeps running meanwhile. It stays off libuv's thread pool too, which file system, DNS, crypto and zlib calls share. At most `os.availableParallelism()` renders run at once; the others wait their turn.

```js
const { createRenderer } = require('rustyprint')

const renderer = createRenderer({ concurrency: 2 })
const { pdf } = await renderer.render(html, { signal: AbortSignal.timeout(5000) })
await renderer.close()
```

`createRenderer` makes a renderer with its own workers. A `signal` stops a render that is still waiting for a worker; one that has started runs to the end, bounded by `limits.timeoutMs`.

`renderSync` renders on the calling thread and blocks the event loop until it is done, for scripts and command-line tools.

## Errors

Every failure is a `RustyprintError`, with a subclass and a `kind` for each kind: `InvalidOptionsError` (`'invalid_options'`), `LimitExceededError` (`'limit_exceeded'`), `TimeoutError` (`'timeout'`), `RecordSetError` (`'record_set'`, a call on a record set that has failed, finished or been aborted) and `InternalError` (`'internal'`, a bug in rustyprint). Arguments that are not valid, such as HTML bytes that are not UTF-8, are a `TypeError`.

## Platforms

The native library, about 31 MB with its fonts, comes in a package of its own for each platform. `rustyprint` lists them all as optional dependencies, and npm installs only the one whose operating system, CPU and C library match the machine:

| Platform | Package |
| --- | --- |
| Linux x64 and arm64, glibc 2.17 or newer | `@rustyprint/linux-x64-gnu`, `@rustyprint/linux-arm64-gnu` |
| Linux x64 and arm64, musl (Alpine) | `@rustyprint/linux-x64-musl`, `@rustyprint/linux-arm64-musl` |
| macOS on Intel and on Apple silicon | `@rustyprint/darwin-x64`, `@rustyprint/darwin-arm64` |
| Windows x64 and on Arm | `@rustyprint/win32-x64-msvc`, `@rustyprint/win32-arm64-msvc` |

If it cannot find the library, rustyprint says which package it expected. The usual reasons:

- **Optional dependencies left out**, by `npm install --omit=optional` or `--no-optional`.
- **`node_modules` or the lockfile from another platform**, such as a lockfile made on macOS and installed in a Linux container by npm before 11.3 ([npm/cli#4828](https://github.com/npm/cli/issues/4828)). Remove both and install again, or install inside the container.
- **A Docker image built on one platform and run on another**: install the dependencies in the image's own platform.

Bun, and npm before 11.11 installing from a lockfile, install both Linux packages, glibc and musl. That does no harm: rustyprint uses the one for the C library it runs on.

koffi has an install script that only checks its prebuilt binary, and npm 12, pnpm and Bun skip dependencies' install scripts unless allowed. rustyprint works either way.

`RUSTYPRINT_LIBRARY` overrides it all: the path of a native library to use, such as one from a `rustyprint-native` archive on the [Releases page](https://github.com/vryan95/rustyprint/releases) or one built with `cargo build --release -p rustyprint-ffi`.

## Develop

```sh
npm install
npm test
npm run check-types
```

`npm test` builds the native library for this platform with Cargo, copies it into `native/`, then runs the tests against it; with `RUSTYPRINT_LIBRARY` set, it uses that library instead. `npm run compile` builds the library on its own, for [demos/node](../../demos/node), an Express app that uses the package.

The release workflow publishes the platform packages from the `rustyprint-native` archives, with [scripts/pack.js](scripts/pack.js), and tries each on a machine of its platform with [scripts/try.js](scripts/try.js) before publishing.

## License

MIT or Apache-2.0, at your option. The bundled Noto fonts are under the SIL Open Font License.
