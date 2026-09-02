# pdf-extractor

Turns a PDF into Markdown, using
[`pdf-inspector`](https://crates.io/crates/pdf-inspector). The source PDF is
opened read-only and never modified.

```rust
let markdown = pdf_extractor::pdf_to_markdown_file("book.pdf")?;   // writes book.md
let text     = pdf_extractor::pdf_to_markdown("book.pdf")?;        // keeps it in memory
```

There is one code path, whatever kind of PDF you hand it. Pages with a usable
text layer are read from that layer; pages without one — a scan, text drawn as
vectors, a broken font encoding — are rendered and OCR'd, and the two are fused
into a single document. Callers never have to know or decide which kind of PDF
they have.

## Anything more than the default

`Options` is the whole of it, and everything on it has a default that suits an
unknown PDF.

```rust
use pdf_extractor::{Ocr, Options, Progress};

let conversion = Options::new()
    .pages(1..=20)
    .compact(true)              // token-efficient instead of source-faithful
    .ocr(Ocr::Auto)             // Off | Auto | Force
    .ocr_dpi(150.0)
    .progress(|event| {
        if let Progress::OcrPage { done, total } = event {
            eprintln!("OCR: page {done} of {total}");
        }
    })
    .convert("book.pdf")?;

eprintln!("{}", conversion.summary());
for note in conversion.notes() {
    eprintln!("{note}");
}
```

`Conversion` carries the facts — page counts, how many came from OCR, per-stage
timings, pages with tables or columns, pages OCR struggled with — and
`summary()` / `notes()` render those as the lines a command-line tool would
print, so a front end does not have to invent the phrasing.

`Options::survey` is the cheap detection pass on its own: what this PDF is and
which pages have no text layer, without extracting anything. Worth running
first, because it answers the one thing a caller wants before a conversion that
could take minutes — whether this is going to be a minutes-long conversion.

Errors are an enum, not a sentence to grep: `Error::Encrypted` is where a caller
prompts for a password, `Error::NoText` is a scanned document that OCR was not
allowed to read, and `Error::Pdf` carries an actionable hint when there is one.

## Being a host

OCR of a long scan is spread across worker processes, which are *this same
executable* re-invoked. That costs a binary one line, before it parses its own
command line:

```rust
fn main() -> ExitCode {
    if let Some(code) = pdf_extractor::run_worker_if_spawned() {
        return code;
    }
    // ... the program's own arguments, from here on.
}
```

Skip it and everything still works single-process once
`Options::ocr_jobs(Some(1))` is set; leave it out at the default and a worker
will report that the host never gave it its turn, by name. `examples/convert.rs`
is the smallest complete program that does it right.

One other thing belongs in `main`: `pdf_extractor::init()`, which finds the two
OCR shared libraries and points the environment at them. A conversion calls it
itself, but it writes process environment variables, which is only sound while
the process is single-threaded — so a program that spawns threads of its own
should call it first, while there provably is only one.

## OCR

OCR is on by default and costs nothing on a PDF that does not need it: PDFium
and ONNX Runtime are `dlopen`ed, and the model set is downloaded, only when a
page is actually routed to OCR. A text PDF converts in milliseconds and never
touches either library.

- **The 300 DPI default is the slow part.** It costs roughly four times the
  150 DPI render and recognition work, but at 150 headings and body text get
  conflated on a mediocre scan and the document loses its structure. Drop to
  `ocr_dpi(150.0)` if you would rather have speed, or use a page selection to
  try a few pages before committing to a long book.
- **OCR runs one page per call, spread across processes.** `pdf-inspector`
  1.17.0 deadlocks when asked to recognize several pages at once on an 8+ core
  machine — a rayon work-stealing re-entrancy against `oar-ocr`'s non-reentrant
  ONNX session mutex. A 595-page scan reliably hung part way through. One page
  per call is the one shape that cannot hit it, but on its own it is also slow
  for a reason unrelated to the work: a single-page request is served by worker
  0 alone on two intra-op threads, so a 24-page slice spent 37 s of wall clock
  on 73 s of CPU — two cores busy out of fourteen. The routed pages are
  therefore fanned out across worker processes, which share no engine, no
  sessions and no locks, and so cannot reconstruct the deadlock. That slice
  drops to 10 s, and the whole 595-page book from 15 min 21 s to 3 min 41 s,
  for byte-identical Markdown. `src/convert.rs` and `src/worker.rs` carry the
  full diagnosis.
- **`ocr_jobs` sizes that fan-out.** The default is half the machine's cores,
  capped at 8 and capped again by memory. A worker resides at roughly 2 GB —
  `pdf-inspector` sizes its engine to the whole machine and builds three
  detector/recognizer pairs, of which a one-page request only ever uses the
  first, and nothing exposes that — so seven of them peaked at 14.9 GB on the
  595-page book. Lower it to trade speed back for memory; `Some(1)` puts
  everything in a single process.
- **Recognition accuracy is bounded by the scan.** A poor source produces the
  classic OCR confusions (`elcctronic` for `electronic`) that no amount of DPI
  fixes. When local OCR is weak on a page, `Conversion::pages_low_confidence`
  says which.
- **`Ocr::Off`** skips OCR entirely. On a fully scanned document that leaves
  nothing to write, and the conversion reports `Error::NoText` rather than
  handing back an empty string.

## Setup, and why there isn't any

Two dependencies are fetched rather than declared, because neither is a Rust
crate: `build.rs` runs `scripts/fetch-ocr-runtime.sh` to put PDFium and ONNX
Runtime in `vendor/` if it cannot already find them, and `pdf-inspector`
downloads the ~31 MB PP-OCRv6 Small model set into its own cache on the first
OCR run.

`src/runtime.rs` then locates the two libraries at startup and fills in
`PDFIUM_LIB_PATH` and `ORT_DYLIB_PATH` itself, searching `vendor/`, the
directory holding the binary, and the usual package-manager prefixes. Setting
either variable by hand overrides the search, so a system-wide install (`brew
install onnxruntime`, PDFium on the loader path) works too.

To build without the download, set `PDF_EXTRACTOR_SKIP_VENDOR=1`. The build
still succeeds; only OCR fails, and only if something actually needs it. The
workspace [README](../../README.md#setup-and-why-there-isnt-any) has the rest.
