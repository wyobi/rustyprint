/* rustyprint: render HTML and CSS to PDF. */

#ifndef RUSTYPRINT_H
#define RUSTYPRINT_H

/* Generated from crates/rustyprint-ffi/src/lib.rs by cbindgen; do not edit. Run `cargo run -p rustyprint-ffi --example header` to regenerate it. */

#include <stddef.h>
#include <stdint.h>

/**
 * What a call did. A render's status is also its result's.
 */
enum rp_status
#if defined(__cplusplus) || __STDC_VERSION__ >= 202311L
  : int32_t
#endif // defined(__cplusplus) || __STDC_VERSION__ >= 202311L
 {
  /**
   * The call succeeded.
   */
  RP_STATUS_OK = 0,
  /**
   * An argument is null where it may not be, or is not UTF-8.
   */
  RP_STATUS_INVALID_ARGUMENT = 1,
  /**
   * The JSON options are not valid.
   */
  RP_STATUS_INVALID_OPTIONS = 2,
  /**
   * The document exceeds one of the limits in the options.
   */
  RP_STATUS_LIMIT_EXCEEDED = 3,
  /**
   * Rendering took longer than the deadline in the options.
   */
  RP_STATUS_TIMEOUT = 4,
  /**
   * A bug in rustyprint, such as a caught panic.
   */
  RP_STATUS_INTERNAL = 5,
  /**
   * The record set cannot go on: it did not open, a record failed earlier or it is
   * finished.
   */
  RP_STATUS_RECORD_SET = 6,
};
#ifndef __cplusplus
#if __STDC_VERSION__ >= 202311L
typedef enum rp_status rp_status;
#else
typedef int32_t rp_status;
#endif // __STDC_VERSION__ >= 202311L
#endif // __cplusplus

/**
 * A PDF file printed one record at a time, which hands over the bytes of the file as they
 * are written. Made by `rp_record_set_open` and freed by `rp_record_set_free`.
 */
typedef struct rp_record_set rp_record_set;

/**
 * What a render gave: a PDF and its warnings, or an error. Made by `rp_render`, or by
 * `rp_content_width` with a width in place of the PDF, and freed by `rp_result_free`.
 */
typedef struct rp_result rp_result;

/**
 * Options and assets for rendering documents. Made by `rp_request_new` and freed by
 * `rp_request_free`.
 */
typedef struct rp_request rp_request;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * The library's version, such as `1.2.0`, as a static string.
 */
const char *rp_version(void);

/**
 * Makes a request from JSON options: `len` bytes of UTF-8 at `options_json`, in the format of
 * `schema/render-options.schema.json`. A length of zero gives the default options.
 *
 * Invalid options still give a request; rendering with it fails with
 * `RP_STATUS_INVALID_OPTIONS` and says why. Returns null only if the library fails.
 *
 * # Safety
 *
 * Unless `len` is zero, `options_json` is null or points to `len` readable bytes.
 */
struct rp_request *rp_request_new(const char *options_json, size_t len);

/**
 * Adds a file the document can refer to, such as an image, stylesheet or font: `len` bytes at
 * `data`, under `path`, the NUL-terminated UTF-8 path the document uses, such as
 * `img/logo.png`. The data is copied. A path added again replaces the earlier file.
 *
 * Returns `RP_STATUS_INVALID_ARGUMENT` if `request` or `path` is null, `path` is not UTF-8,
 * or `data` is null with a length that is not zero.
 *
 * # Safety
 *
 * `request` is null or a request no other thread is using. `path` is null or a NUL-terminated
 * string. Unless `len` is zero, `data` is null or points to `len` readable bytes.
 */
rp_status rp_request_add_asset(struct rp_request *request,
                               const char *path,
                               const uint8_t *data,
                               size_t len);

/**
 * Renders `len` bytes of UTF-8 HTML at `html` with the request's options and assets, and
 * stores the result in `*out`, which the caller frees with `rp_result_free`. Returns the
 * result's status: `RP_STATUS_OK` with a PDF, or an error with a message.
 *
 * Every call with a non-null `out` stores a result there, even when it fails, unless the
 * library cannot allocate one. A null `out` returns `RP_STATUS_INVALID_ARGUMENT`.
 *
 * # Safety
 *
 * `request` is null or a live request that no thread is adding assets to. Unless `len` is
 * zero, `html` is null or points to `len` readable bytes. `out` is null or writable.
 */
rp_status rp_render(const struct rp_request *request,
                    const char *html,
                    size_t len,
                    struct rp_result **out);

/**
 * Measures the width, in CSS pixels, of the content of `len` bytes of UTF-8 HTML at `html` in
 * a browser window `window_width` by `window_height` CSS pixels in size: the window's width,
 * or more when content reaches past its right edge. The document is laid out for the screen,
 * on one continuous page. Of the request's options, the stylesheet, the limits and the
 * assets apply.
 *
 * Stores the result in `*out`, which the caller frees with `rp_result_free`, and returns its
 * status: `RP_STATUS_OK` with a width that `rp_result_content_width` gives, or an error with
 * a message. A null `out` returns `RP_STATUS_INVALID_ARGUMENT`.
 *
 * # Safety
 *
 * As for [`rp_render`].
 */
rp_status rp_content_width(const struct rp_request *request,
                           const char *html,
                           size_t len,
                           double window_width,
                           double window_height,
                           struct rp_result **out);

/**
 * The width `rp_content_width` measured, in CSS pixels. Zero if it failed, for a result of
 * `rp_render` and for a null result.
 *
 * # Safety
 *
 * `result` is null or a live result.
 */
double rp_result_content_width(const struct rp_result *result);

/**
 * The result's status, the one `rp_render` returned. `RP_STATUS_INVALID_ARGUMENT` for a null
 * result.
 *
 * # Safety
 *
 * `result` is null or a live result.
 */
rp_status rp_result_status(const struct rp_result *result);

/**
 * The PDF, with its length in bytes stored in `*len` unless `len` is null. Null, with a
 * length of zero, if the render failed. The bytes live as long as the result.
 *
 * # Safety
 *
 * `result` is null or a live result. `len` is null or writable.
 */
const uint8_t *rp_result_pdf(const struct rp_result *result, size_t *len);

/**
 * The render's warnings, as a NUL-terminated JSON array of strings such as
 * `` ["style.css:3: unsupported property `colr`"] ``, with its length in bytes, not counting the NUL,
 * stored in `*len` unless `len` is null. `[]` if there are none or the render failed; null
 * only for a null result. The string lives as long as the result.
 *
 * # Safety
 *
 * `result` is null or a live result. `len` is null or writable.
 */
const char *rp_result_warnings_json(const struct rp_result *result,
                                    size_t *len);

/**
 * Why the render failed, as a NUL-terminated message, with its length in bytes, not counting
 * the NUL, stored in `*len` unless `len` is null. Null, with a length of zero, if the render
 * succeeded. The string lives as long as the result.
 *
 * # Safety
 *
 * `result` is null or a live result. `len` is null or writable.
 */
const char *rp_result_error(const struct rp_result *result, size_t *len);

/**
 * Opens a record set, which prints many documents as one PDF file one record at a time, with
 * the request's options and assets, which are copied: the request may be freed or used for
 * other renders afterwards. Every record is a document of its own with those options; page
 * numbers run through the set, and `counter(pages)` is the number of pages of the whole set.
 * Stores the set in `*out`, which the caller frees with `rp_record_set_free`, and returns
 * `RP_STATUS_OK`, or why the set did not open, with a message that `rp_record_set_error`
 * gives.
 *
 * Every call with a non-null `out` stores a set there, even when it fails, unless the
 * library cannot allocate one. A null `out` returns `RP_STATUS_INVALID_ARGUMENT`.
 *
 * The set lays its records out on a thread of its own, which ends when the set is finished,
 * when a record fails and when the set is freed.
 *
 * # Safety
 *
 * `request` is null or a live request that no thread is adding assets to. `out` is null or
 * writable.
 */
rp_status rp_record_set_open(const struct rp_request *request, struct rp_record_set **out);

/**
 * Prints `len` bytes of UTF-8 HTML at `html` as the next record of the set, on the pages
 * after those of the records so far. Returns `RP_STATUS_OK`, with the bytes of the pages
 * that are written ready for `rp_record_set_take_output`, the record's pages for
 * `rp_record_set_added_pages` and its warnings for `rp_record_set_warnings_json`; or why it
 * failed, with a message that `rp_record_set_error` gives. The last page of the record is
 * open for a record that continues it (`rp_record_set_add_continuing`): what it paints is
 * written with the next record or when the set is finished. Besides that page and the
 * bytes, nothing of the record is kept, so a caller that takes the bytes after every
 * record has the library hold one record at a time.
 *
 * A record that fails ends the set: what was written is not a whole PDF file, and every
 * later call fails with `RP_STATUS_RECORD_SET`. So does a call on a set that did not open
 * or is finished. HTML that is null or not UTF-8 is `RP_STATUS_INVALID_ARGUMENT`, and
 * leaves the set as it was. A null set returns `RP_STATUS_INVALID_ARGUMENT`.
 *
 * # Safety
 *
 * `set` is null or a live record set no other thread is using. Unless `len` is zero, `html`
 * is null or points to `len` readable bytes.
 */
rp_status rp_record_set_add(struct rp_record_set *set, const char *html, size_t len);

/**
 * Prints `len` bytes of UTF-8 HTML at `html` as more of the record before it: its content
 * follows that record's on the same page, as if it were at the end of that record's
 * `<body>`, so a document can be printed a piece at a time and come out as it does printed
 * whole. A piece that starts with `<table data-rp-continue>` adds the rows of that table to
 * the open table, which keeps its columns, header and footer: the first `<table>` among the
 * open elements, which are `<body>` and the last child of an open element when nothing but
 * white space and comments follows it, at any depth. It and the elements it is in have to
 * be blocks among blocks (`display: block`, `flow-root` or `list-item`, not floated or
 * absolutely positioned, no content-sized `width`, not in a flex, grid, table or
 * multi-column box), or the call fails with `RP_STATUS_RECORD_SET`. An element with
 * `data-rp-continue` at the start of a piece's `<body>`, or at the start of such an
 * element, stands for the open element of its name at that place, and its content is added
 * to that one's: so the rest of a document after a table is one start tag with
 * `data-rp-continue` per element the table is in, then the HTML after its `</table>`.
 * `rp_record_set_added_pages` is then the pages the piece started, which is none when it
 * fits on the open page. Everything else is as `rp_record_set_add` has it; the first record
 * of a set can be added either way.
 *
 * # Safety
 *
 * `set` is null or a live record set no other thread is using. Unless `len` is zero, `html`
 * is null or points to `len` readable bytes.
 */
rp_status rp_record_set_add_continuing(struct rp_record_set *set, const char *html, size_t len);

/**
 * Ends the file: writes what the whole file shares, such as its fonts, its page tree and
 * the number of pages that `counter(pages)` prints. Returns `RP_STATUS_OK`, with those last
 * bytes ready for `rp_record_set_take_output`; or why it failed, with a message that
 * `rp_record_set_error` gives. A set without records is a file without pages.
 *
 * The set takes no more records, and still has to be freed. A null set returns
 * `RP_STATUS_INVALID_ARGUMENT`.
 *
 * # Safety
 *
 * `set` is null or a live record set no other thread is using.
 */
rp_status rp_record_set_finish(struct rp_record_set *set);

/**
 * Takes the bytes of the file written since the last take: those that follow the bytes
 * taken before. Their length is stored in `*len` unless `len` is null. Null, with a length
 * of zero, when nothing was written since, and for a null set.
 *
 * The bytes live until the next `rp_record_set_add`, `rp_record_set_finish` or
 * `rp_record_set_take_output` with the set, or until it is freed; the set holds them
 * nowhere else. A caller takes them after every record and after finishing, in that order,
 * and has the whole file; bytes not taken wait in the set and come with the next take.
 *
 * # Safety
 *
 * `set` is null or a live record set no other thread is using. `len` is null or writable.
 */
const uint8_t *rp_record_set_take_output(struct rp_record_set *set, size_t *len);

/**
 * How many pages the record added last took. Zero if it failed, after
 * `rp_record_set_finish` and for a null set.
 *
 * # Safety
 *
 * `set` is null or a live record set no other thread is using.
 */
size_t rp_record_set_added_pages(const struct rp_record_set *set);

/**
 * The warnings of the record added last, as a NUL-terminated JSON array of strings, with
 * its length in bytes, not counting the NUL, stored in `*len` unless `len` is null. `[]` if
 * there are none, if the record failed and after `rp_record_set_finish`; null only for a
 * null set. The string lives until the next `rp_record_set_add` or `rp_record_set_finish`
 * with the set, or until it is freed.
 *
 * # Safety
 *
 * `set` is null or a live record set no other thread is using. `len` is null or writable.
 */
const char *rp_record_set_warnings_json(const struct rp_record_set *set, size_t *len);

/**
 * Why the last `rp_record_set_open`, `rp_record_set_add` or `rp_record_set_finish` with the
 * set failed, as a NUL-terminated message, with its length in bytes, not counting the NUL,
 * stored in `*len` unless `len` is null. Null, with a length of zero, if it succeeded. The
 * string lives until the next `rp_record_set_add` or `rp_record_set_finish` with the set,
 * or until it is freed.
 *
 * # Safety
 *
 * `set` is null or a live record set no other thread is using. `len` is null or writable.
 */
const char *rp_record_set_error(const struct rp_record_set *set, size_t *len);

/**
 * How many pages the set has so far: after `rp_record_set_finish`, the pages of the file.
 * Zero for a null set.
 *
 * # Safety
 *
 * `set` is null or a live record set no other thread is using.
 */
size_t rp_record_set_pages(const struct rp_record_set *set);

/**
 * How many records the set has so far: after `rp_record_set_finish`, the records of the
 * file. Zero for a null set.
 *
 * # Safety
 *
 * `set` is null or a live record set no other thread is using.
 */
size_t rp_record_set_records(const struct rp_record_set *set);

/**
 * Frees a record set, with the bytes, warnings and error it holds, and ends its thread. A
 * set may be freed at any point: one that is not finished is given up, and what was written
 * is not a whole PDF file. Null does nothing.
 *
 * # Safety
 *
 * `set` is null or a live record set from `rp_record_set_open`, which no other thread is
 * using and which is not used again.
 */
void rp_record_set_free(struct rp_record_set *set);

/**
 * Frees a request. Null does nothing.
 *
 * # Safety
 *
 * `request` is null or a live request from `rp_request_new`, which no other thread is using
 * and which is not used again.
 */
void rp_request_free(struct rp_request *request);

/**
 * Frees a result, and with it the PDF, warnings and error it holds. Null does nothing.
 *
 * # Safety
 *
 * `result` is null or a live result from `rp_render` or `rp_content_width`, which is not used
 * again.
 */
void rp_result_free(struct rp_result *result);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* RUSTYPRINT_H */
