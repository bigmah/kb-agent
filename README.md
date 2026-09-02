# kb-agent

Turns documents into Markdown, and Markdown into shorter Markdown. Today that
means PDFs, including scanned ones, and LLM summaries of what comes out.

```bash
cargo build --release
./target/release/kb-agent book.pdf              # writes book.md
```

There is one command and one code path, whatever kind of PDF you hand it. Pages
with a usable text layer are read from that layer; pages without one — a scan,
text drawn as vectors, a broken font encoding — are rendered and OCR'd, and the
two are fused into a single document. Nothing upstream has to know or decide
which kind of PDF it has.

## Layout

| | |
| --- | --- |
| [`crates/pdf-extractor`](crates/pdf-extractor) | PDF in, Markdown out, OCR included. |
| [`crates/agent`](crates/agent) | Markdown in, a shorter Markdown summary out, via an LLM. |
| [`crates/kb-agent`](crates/kb-agent) | The command. Flags, files, and everything said on the way. |
| `samples/` | Untracked scratch for trying the tools on real documents — see below. |

The split is the point: each library decides nothing about where output goes or
what gets printed, so anything that wants the work can have it without
inheriting a command-line tool's opinions.

```rust
let markdown = pdf_extractor::pdf_to_markdown_file("book.pdf")?;   // book.md
let summary  = agent::summarize_markdown_file("book.md").await?;   // book_summary.md
```

Each crate's README covers the rest of its API:
[pdf-extractor](crates/pdf-extractor/README.md) for page selection, OCR and the
one line a host binary owes it; [agent](crates/agent/README.md) for providers,
the context budget and cost; [kb-agent](crates/kb-agent/README.md) for the
flags.

## Setup, and why there isn't any

Three things the OCR path needs are provisioned automatically:

| Thing | How it gets there |
| --- | --- |
| rustc 1.95 (the OCR stack's minimum) | `rust-toolchain.toml` — rustup fetches it on the first build, leaving your default toolchain alone |
| PDFium and ONNX Runtime shared libraries | `crates/pdf-extractor/build.rs` runs `scripts/fetch-ocr-runtime.sh` into `vendor/` if it cannot already find them |
| PP-OCRv6 Small model weights (~31 MB) | `pdf-inspector` downloads and checksum-verifies them into `~/Library/Caches/pdf-inspector` the first time OCR runs |

So `cargo build --release` is the whole of it. To build without the download —
on a machine that already has the libraries, or one that is offline — set
`PDF_EXTRACTOR_SKIP_VENDOR=1`. The build still succeeds; only OCR fails, and
only if something actually needs it.

## Tests

```bash
cargo test --workspace
```

Unit tests only, and fast — nothing here needs an API key, a GPU, or a network.
They cover argument parsing, the worker wire format, page assembly, the
context-budget check and the reporting lines.

The end-to-end checks need a real document, and real documents are too big to
carry in git, so `samples/` is ignored rather than committed. Put any PDF in
there and convert it:

```bash
./target/release/kb-agent samples/your.pdf     # writes samples/your.md
```

Two things are worth confirming by hand on a scanned PDF, because no unit test
can: that `--ocr-jobs 1` and the default parallel run produce byte-identical
Markdown, and that the summary line's page counts match the document.
