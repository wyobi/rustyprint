/* Renders a document through the C ABI, as a C program using the library would.
 * Exits with 0 when every check passes. Built and run by tests/c_smoke.rs, and by the release
 * workflow against each library it builds. */

#include <stdio.h>
#include <string.h>

#include "rustyprint.h"

static int failures = 0;

#define CHECK(condition)                                                    \
    do {                                                                    \
        if (!(condition)) {                                                 \
            fprintf(stderr, "%s:%d: failed: %s\n", __FILE__, __LINE__,      \
                    #condition);                                            \
            failures++;                                                     \
        }                                                                   \
    } while (0)

static void renders_with_options_and_an_asset(void) {
    const char *options = "{\"page_size\": \"A5\", \"title\": \"Smoke test\"}";
    const char *html =
        "<link rel=\"stylesheet\" href=\"style.css\">"
        "<h1>Hello from C</h1><p>colr is not a property</p>";
    const char *css = "h1 { color: navy } p { colr: red }";

    rp_request *request = rp_request_new(options, strlen(options));
    CHECK(request != NULL);
    CHECK(rp_request_add_asset(request, "style.css", (const uint8_t *)css,
                               strlen(css)) == RP_STATUS_OK);

    rp_result *result = NULL;
    rp_status status = rp_render(request, html, strlen(html), &result);
    CHECK(status == RP_STATUS_OK);
    CHECK(result != NULL);
    CHECK(rp_result_status(result) == RP_STATUS_OK);

    size_t len = 0;
    const uint8_t *pdf = rp_result_pdf(result, &len);
    CHECK(pdf != NULL);
    CHECK(len > 5 && memcmp(pdf, "%PDF-", 5) == 0);

    const char *warnings = rp_result_warnings_json(result, &len);
    CHECK(warnings != NULL && warnings[0] == '[' && strlen(warnings) == len);
    CHECK(strstr(warnings, "colr") != NULL);
    CHECK(rp_result_error(result, NULL) == NULL);

    rp_result_free(result);
    rp_request_free(request);
}

static void reports_errors(void) {
    const char *options = "{\"page_size\": \"B99\"}";
    rp_request *request = rp_request_new(options, strlen(options));
    rp_result *result = NULL;
    rp_status status = rp_render(request, "<p>x</p>", 8, &result);
    CHECK(status == RP_STATUS_INVALID_OPTIONS);

    size_t len = 1;
    CHECK(rp_result_pdf(result, &len) == NULL && len == 0);
    const char *error = rp_result_error(result, &len);
    CHECK(error != NULL && strlen(error) == len && len > 0);
    CHECK(strcmp(rp_result_warnings_json(result, NULL), "[]") == 0);

    rp_result_free(result);
    rp_request_free(request);
}

/* Appends the bytes the set wrote since the last take to the file. */
static size_t take_output(rp_record_set *set, FILE *file) {
    size_t len = 0;
    const uint8_t *bytes = rp_record_set_take_output(set, &len);
    CHECK((bytes != NULL) == (len > 0));
    if (len > 0) {
        CHECK(fwrite(bytes, 1, len, file) == len);
    }
    return len;
}

/* Prints three records as one PDF file at `path`, taking the bytes after every record. */
static void prints_a_record_set(const char *path) {
    const char *options = "{\"page_size\": \"A5\", \"limits\": {\"max_total_pages\": 10}}";
    const char *records[] = {
        "<h1>Record 1</h1><p>colr is not a property</p><style>p { colr: red }</style>",
        "<h1>Record 2</h1><p style=\"break-before: page\">Its second page</p>",
        "<h1>Record 3</h1>",
    };
    const size_t pages[] = {1, 2, 1};
    char header[5] = {0};
    size_t len = 0;
    int i;

    rp_request *request = rp_request_new(options, strlen(options));
    rp_record_set *set = NULL;
    CHECK(rp_record_set_open(request, &set) == RP_STATUS_OK);
    CHECK(set != NULL);
    CHECK(rp_record_set_error(set, NULL) == NULL);
    /* The set has its own copy of the options. */
    rp_request_free(request);

    FILE *file = fopen(path, "wb");
    CHECK(file != NULL);
    if (file == NULL) {
        rp_record_set_free(set);
        return;
    }
    for (i = 0; i < 3; i++) {
        CHECK(rp_record_set_add(set, records[i], strlen(records[i])) == RP_STATUS_OK);
        CHECK(rp_record_set_added_pages(set) == pages[i]);
        const char *warnings = rp_record_set_warnings_json(set, &len);
        CHECK(warnings != NULL && strlen(warnings) == len);
        CHECK((strstr(warnings, "colr") != NULL) == (i == 0));
        /* Every record's pages are written before the next record is added. */
        CHECK(take_output(set, file) > 0);
        CHECK(take_output(set, file) == 0);
    }
    CHECK(rp_record_set_finish(set) == RP_STATUS_OK);
    CHECK(take_output(set, file) > 0);
    CHECK(rp_record_set_pages(set) == 4);
    CHECK(rp_record_set_records(set) == 3);
    CHECK(fclose(file) == 0);

    /* A finished set takes no more records. */
    CHECK(rp_record_set_add_continuing(set, "<p>x</p>", 8) == RP_STATUS_RECORD_SET);
    CHECK(rp_record_set_add(set, "<p>x</p>", 8) == RP_STATUS_RECORD_SET);
    const char *error = rp_record_set_error(set, &len);
    CHECK(error != NULL && strlen(error) == len && len > 0);
    CHECK(rp_record_set_records(set) == 3);
    rp_record_set_free(set);

    file = fopen(path, "rb");
    CHECK(file != NULL);
    if (file != NULL) {
        CHECK(fread(header, 1, sizeof header, file) == sizeof header);
        CHECK(memcmp(header, "%PDF-", sizeof header) == 0);
        fclose(file);
    }
}

static void gives_up_a_record_set(void) {
    rp_request *request = rp_request_new(NULL, 0);
    rp_record_set *set = NULL;
    CHECK(rp_record_set_open(request, &set) == RP_STATUS_OK);
    CHECK(rp_record_set_add(set, "<p>x</p>", 8) == RP_STATUS_OK);
    /* Freed in the middle, with bytes not taken. */
    rp_record_set_free(set);
    rp_request_free(request);

    const char *options = "{\"page_size\": \"B99\"}";
    request = rp_request_new(options, strlen(options));
    set = NULL;
    CHECK(rp_record_set_open(request, &set) == RP_STATUS_INVALID_OPTIONS);
    CHECK(set != NULL && rp_record_set_error(set, NULL) != NULL);
    CHECK(rp_record_set_add(set, "<p>x</p>", 8) == RP_STATUS_RECORD_SET);
    rp_record_set_free(set);
    rp_request_free(request);
}

/* The record set's PDF file is written to the path given as the first argument and kept, or
 * to the working directory and removed. */
int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "smoke-record-set.pdf";
    printf("rustyprint %s\n", rp_version());
    renders_with_options_and_an_asset();
    reports_errors();
    prints_a_record_set(path);
    gives_up_a_record_set();
    if (argc <= 1) {
        remove(path);
    }
    rp_request_free(NULL);
    rp_result_free(NULL);
    rp_record_set_free(NULL);
    if (failures > 0) {
        fprintf(stderr, "%d checks failed\n", failures);
        return 1;
    }
    printf("ok\n");
    return 0;
}
