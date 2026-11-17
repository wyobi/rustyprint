/// <reference types="node" />

import type { Readable } from 'node:stream'

/** A page size as a width and height, each a CSS length such as `100mm`. */
export interface PageSizeDimensions {
  width: string
  height: string
}

/** Hosts that absolute URLs may be fetched from. Without them, nothing is fetched. */
export interface RemoteOptions {
  /** Host names, such as `cdn.example.com`, or `*.example.com` for its subdomains. */
  allowedHosts: string[]
  /** Also fetch plain http URLs. Default: false. */
  allowHttp?: boolean
  /** How long one fetch may take. Default: 10000. */
  timeoutMs?: number
  /** The largest file fetched. Default: 10 MB. */
  maxBytes?: number
  /** Redirects followed per fetch. Default: 5. */
  maxRedirects?: number
  /** Files fetched per render; the rest are not loaded. Default: 100. */
  maxFetches?: number
}

/** Bounds on the work of the render. Exceeding one fails the render, except that larger images are skipped with a warning. */
export interface Limits {
  /** Size of the HTML in bytes. Default: 20 MB. */
  maxInputBytes?: number
  /** DOM nodes across the document and its iframe documents. Default: 500000. */
  maxNodes?: number
  /** How deeply elements nest. Default: 512. */
  maxDepth?: number
  /** Pages in the PDF. Default: 2000. */
  maxPages?: number
  /** Pages in the PDF of a whole record set, whose records each have `maxPages` at the most. Default: 200000. */
  maxTotalPages?: number
  /** Width times height of an image. Default: 100000000. */
  maxImagePixels?: number
  /** How long the render may take, or null for no deadline. Default: 60000. */
  timeoutMs?: number | null
}

/** Asset contents: a string is encoded as UTF-8, bytes are used as they are. */
export type AssetData = string | ArrayBuffer | NodeJS.ArrayBufferView

/**
 * The options of schema/render-options.schema.json, with camelCase names, and the assets.
 * An option left out keeps its default.
 */
export interface RenderOptions {
  /** A3, A4 (the default), A5, Letter or Legal, in any case, or a width and height. */
  pageSize?: 'A3' | 'A4' | 'A5' | 'Letter' | 'Legal' | (string & {}) | PageSizeDimensions
  /** Turns the page so that it is wider than it is tall. Default: false. */
  landscape?: boolean
  /** Page margins like the CSS margin shorthand, such as `15mm` or `10mm 20mm`. Default: 2cm. */
  margin?: string
  /**
   * The scale of the document on the page, from 0.1 to 2, as the scale of a browser's print.
   * The document is laid out on a page area that much wider or narrower. Default: 1.
   */
  scale?: number
  /**
   * Shrinks a document laid out wider than the page area to fit it, as a browser's print does,
   * by up to a third; what is wider still is cut off. Default: false.
   */
  shrinkToFit?: boolean
  /** The PDF's title. Default: the document's `<title>`. */
  title?: string
  /** A stylesheet applied after the document's own styles. */
  stylesheet?: string
  remote?: RemoteOptions
  limits?: Limits
  /**
   * The files the document refers to by relative or root-relative URL, by the path it uses
   * without a leading slash (`/css/app.css` is `css/app.css`).
   */
  assets?: Record<string, AssetData> | Map<string, AssetData>
}

export interface AsyncRenderOptions extends RenderOptions {
  /** Stops the render if it has not started yet. A render that has started runs to the end, bounded by `limits.timeoutMs`. */
  signal?: AbortSignal
}

export interface RenderOutput {
  /** The PDF. */
  pdf: Buffer
  /** Problems found in the document's CSS and assets, such as "invoice.css:12: unsupported property `colr`". */
  warnings: string[]
}

/** The size of a browser window in CSS pixels. */
export interface WindowSize {
  width: number
  height: number
}

export interface RendererOptions {
  /** How many renders run at once, each on its own worker thread. Default: `os.availableParallelism()`. */
  concurrency?: number
}

/** The options of a record set, which every record is printed with. */
export interface RecordSetOptions extends RenderOptions {
  /** Stops the set's wait for a worker. A call that has started runs to the end. */
  signal?: AbortSignal
}

/** How a record is added to a set. */
export interface AddOptions {
  /**
   * Whether the record is more of the record before it, instead of a document of its own: its
   * content follows that record's on the same page, as if it were at the end of that record's
   * `<body>`, so a document printed a piece at a time comes out as it does printed whole. A
   * piece that starts with `<table data-rp-continue>` adds the rows of that table's `<tbody>` to
   * the table that the record before ended with, which keeps its column widths, header and
   * footer; its own `<colgroup>`, `<thead>` and `<tfoot>` are not used. The styles are those of
   * the record that started. Default: false, so the record starts on a new page.
   */
  continue?: boolean
}

export interface RecordOutput {
  /**
   * The bytes of the PDF this call wrote: they follow the bytes of the calls before. The last
   * page of a record is open for a record that continues it: what it paints is written by the
   * next call.
   */
  pdf: Buffer
  /**
   * How many pages the record added to the set: the pages it took, or for a record that
   * continues, the pages it started, which is none when it fits on the open page.
   */
  pages: number
  /** Problems found in the record's CSS and assets. */
  warnings: string[]
}

export interface RecordSetSummary {
  /** The last bytes of the PDF: what the whole file shares, such as its fonts and page tree. */
  pdf: Buffer
  /** How many pages the PDF has. */
  pages: number
  /** How many records were added. */
  records: number
}

/**
 * Many documents printed as one PDF, one record at a time. The bytes that `add` and `finish`
 * resolve to, in the order of the calls, are the PDF file. Calls run one after another: a call
 * made while another runs waits for it.
 *
 * One worker of the renderer prints the whole set, from the first call until the set is
 * finished, aborted or fails, and renders nothing else meanwhile. A call that fails ends the
 * set: the bytes so far are not a whole PDF, and later calls reject with a `RecordSetError`.
 */
export interface RecordSet {
  /**
   * Prints a document as the next record, on the pages after those printed so far, or with
   * `{ continue: true }` as more of the record before it, on that record's last page.
   */
  add (html: string | Uint8Array | ArrayBuffer, options?: AddOptions): Promise<RecordOutput>
  /** Ends the PDF. A set without records is a PDF without pages. */
  finish (): Promise<RecordSetSummary>
  /** Gives the set up and frees its worker. Does nothing to a set that has ended; never rejects. */
  abort (): Promise<void>
}

/** A record's HTML with how it is added, for a record that continues the one before it. */
export interface NextRecordOptions extends AddOptions {
  html: string | Uint8Array | ArrayBuffer
}

/**
 * A record's HTML, or its HTML with how it is added, or null or undefined when there are no
 * more records.
 */
export type NextRecord = string | Uint8Array | ArrayBuffer | NextRecordOptions | null | undefined

export interface ToReadableOptions {
  /**
   * How many bytes the stream holds before it stops asking for records. Default: 0, so a
   * record is printed only when the consumer asks for more.
   */
  highWaterMark?: number
}

export interface Renderer {
  /** Renders an HTML document to PDF on one of this renderer's workers. */
  render (html: string | Uint8Array | ArrayBuffer, options?: AsyncRenderOptions): Promise<RenderOutput>
  /** Measures the width of a document's content on one of this renderer's workers, as `contentWidth` does. */
  contentWidth (html: string | Uint8Array | ArrayBuffer, window: WindowSize, options?: AsyncRenderOptions): Promise<number>
  /** Makes a record set that one of this renderer's workers prints, as `createRecordSet` does. */
  createRecordSet (options?: RecordSetOptions): RecordSet
  /** Stops the workers. Renders still waiting are refused, and open record sets are aborted. */
  close (): Promise<void>
}

/**
 * Renders an HTML document to PDF on a render worker. The event loop keeps running meanwhile,
 * and so do file system, DNS and crypto calls.
 */
export function render (html: string | Uint8Array | ArrayBuffer, options?: AsyncRenderOptions): Promise<RenderOutput>

/** Renders an HTML document to PDF on the calling thread, blocking the event loop until it is done. */
export function renderSync (html: string | Uint8Array | ArrayBuffer, options?: RenderOptions): RenderOutput

/**
 * Measures, on a render worker, the width in CSS pixels of a document's content in a browser
 * window of the given size: the window's width, or more when content reaches past its right
 * edge. The document is laid out for the screen, on one continuous page. Of the options, the
 * stylesheet, the assets and the limits apply.
 *
 * To fit a wide document on a page, render it with a `scale` of the page area's width divided
 * by this width.
 */
export function contentWidth (html: string | Uint8Array | ArrayBuffer, window: WindowSize, options?: AsyncRenderOptions): Promise<number>

/** Measures the width of a document's content on the calling thread, as `contentWidth` does. */
export function contentWidthSync (html: string | Uint8Array | ArrayBuffer, window: WindowSize, options?: RenderOptions): number

/** Makes a renderer with its own workers and concurrency. */
export function createRenderer (options?: RendererOptions): Renderer

/**
 * Makes a record set: many documents printed as one PDF, one record at a time, so that the
 * whole PDF is never in memory. Every record is a document of its own with the set's options;
 * page numbers run through the set, and `counter(pages)` is the pages of the whole set.
 * `limits` hold for each record, and `limits.maxTotalPages` for the set.
 */
export function createRecordSet (options?: RecordSetOptions): RecordSet

/**
 * The PDF of a record set as a Readable that prints only what is read. Each time the consumer
 * wants more, it calls `nextHtml` for the next record, adds it to the set and pushes its
 * bytes; when `nextHtml` gives null, it finishes the set and pushes the last bytes. A record
 * given as `{ html, continue: true }` continues the record before it. A consumer that stops
 * reading stops the records. An error, or `destroy()`, aborts the set.
 */
export function toReadable (
  set: RecordSet,
  nextHtml: () => NextRecord | Promise<NextRecord>,
  options?: ToReadableOptions
): Readable

/** The native library's version. */
export function nativeVersion (): string

/** The package's version, the same as the native library's. */
export const VERSION: string

/** A render that failed. Each kind of failure has its own subclass. */
export class RustyprintError extends Error {
  readonly kind: 'invalid_options' | 'limit_exceeded' | 'timeout' | 'record_set' | 'internal'
}

/** The options are not valid, such as an unknown option or page size. */
export class InvalidOptionsError extends RustyprintError {
  readonly kind: 'invalid_options'
}

/** The document exceeds one of the limits in the options. */
export class LimitExceededError extends RustyprintError {
  readonly kind: 'limit_exceeded'
}

/** Rendering took longer than the deadline in the options. */
export class TimeoutError extends RustyprintError {
  readonly kind: 'timeout'
}

/** The record set cannot go on: an earlier call on it failed, or it is finished or aborted. */
export class RecordSetError extends RustyprintError {
  readonly kind: 'record_set'
}

/** A bug in rustyprint. */
export class InternalError extends RustyprintError {
  readonly kind: 'internal'
}
