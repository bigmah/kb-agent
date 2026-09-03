# kb-agent

A directory of books and papers as something you can ask. Every document is
read in full before an answer comes back — each by its own request, each in a
context holding nothing else — and the answer is written from what they said.

```bash
cargo build --release
./target/release/kb-agent build library/                      # every PDF → Markdown → summary
./target/release/kb-agent query library/ "What limits the throughput of an order book?"
./target/release/kb-agent chat library/                       # a prompt: questions, and /commands for the rest
```

## Why

An agent given a question and a search box is a very smart person allowed to
google for a minute before answering. Its mechanics are strong; its taste is
not, because taste comes from having read the shelf. The question this repo
asks is what changes when the agent has read the shelf — not skimmed it into
one context, but read every book on it — and whether what comes back gets
better with the size of the shelf rather than with the size of the window.

Prior art puts a fan-out over many contexts at something like 80% of the
fidelity of one context that holds everything. That is fine and expected;
reasoning inside one window beats reasoning across several. The interesting
case is not ten small windows standing in for one large one. It is ten large
windows, or a hundred, or a hundred thousand — whether intelligence here is a
function of the total context brought to bear.

## How

```
library/            ──build──►  library/            ──query──►  .kb-agent/queries/<run>/
  a.pdf                           a.pdf  a.md  a_summary.md         mask.md
  b.pdf                           b.pdf  b.md  b_summary.md         points.raw.md
  c.md                            c.md         c_summary.md         points.md
                                                                    answer.md
```

1. **Convert.** Every PDF becomes Markdown, OCR included. A document that does
   not fit one request is left out, not cut down: a reading of chapter one
   labelled as a reading of the book is worse than no reading.
2. **Summarize.** Every document gets a summary beside it, one request each.
3. **Mask.** For a question, every summary is judged for relevance, one
   request each, so the verdict on one document is not coloured by the
   ninety-nine before it.
4. **Ask.** Every relevant document is read in full and asked the question,
   one request each, told to use the document and nothing else. Each answers
   with a list of self-contained points.
5. **Reduce.** Every pair of points is compared, one request each; groups
   judged the same are merged into one point that keeps everything the group
   said. The long list becomes the refined list.
6. **Answer.** The question is answered from the refined list, in one request,
   with sources named.

The refined list is as much the product as the answer: what a few hundred
books had to say about the question, in a form that fits in someone else's
context. The agent you actually talk to — the one asked to make a repo faster,
or find the holes in an idea — is meant to consume that list and act.

## Layout

| | |
| --- | --- |
| [`crates/pdf-extractor`](crates/pdf-extractor) | PDF in, Markdown out, OCR included. |
| [`crates/agent`](crates/agent) | One LLM request with a fresh context, in each role above: summarize, judge relevance, answer from one document, compare two points, merge them, answer from the list. |
| [`crates/kb`](crates/kb) | The directory as a library: the index by path, building it, and running a question through every document in it. |
| [`crates/kb-agent`](crates/kb-agent) | The command. Flags, files, everything said on the way, and a prompt to say it at. |
| `samples/` | Untracked scratch for trying the tools on real documents — see below. |

The split is the point: each library decides nothing about where output goes or
what gets printed, so anything that wants the work can have it without
inheriting a command-line tool's opinions.

```rust
let markdown = pdf_extractor::pdf_to_markdown_file("book.pdf")?;   // book.md
let summary  = agent::summarize_markdown_file("book.md").await?;   // book_summary.md

let library = kb::Corpus::scan("library/")?;
let result  = library.query("…", &kb::QueryOptions::new()).await?;
```

Each crate's README covers the rest of its API:
[pdf-extractor](crates/pdf-extractor/README.md) for page selection, OCR and the
one line a host binary owes it; [agent](crates/agent/README.md) for the roles,
providers, the context budget and retries; [kb](crates/kb/README.md) for the
index, the stages and what they cost; [kb-agent](crates/kb-agent/README.md)
for the flags and the files a query leaves behind.

## Setup, and why there isn't any

An API key in `OPENAI_API_KEY` (or `ANTHROPIC_API_KEY` with
`--provider anthropic`) is the only thing to provide. Three things the OCR
path needs are provisioned automatically:

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
context-budget check, reading verdicts and bullet lists out of model replies,
the fan-out's ordering and cancellation, the clustering behind the reduction,
the directory scan, and the reporting lines.

The end-to-end checks need real documents and a key, and real documents are
too big to carry in git, so `samples/` is ignored rather than committed. Put
PDFs in a directory and build it:

```bash
./target/release/kb-agent build samples/
./target/release/kb-agent status samples/
./target/release/kb-agent query samples/ "…" --plan     # what would be judged, nothing sent
./target/release/kb-agent chat samples/                 # the same at a prompt: /plan …, /status, /runs
```

Two things are worth confirming by hand on a scanned PDF, because no unit test
can: that `--ocr-jobs 1` and the default parallel run produce byte-identical
Markdown, and that the summary line's page counts match the document.

## What is not here yet

- **The reduction is quadratic.** Every pair is compared because that is the
  design; two hundred points is twenty thousand small requests. The obvious
  next steps — merging within a source first, or an embedding pass to skip
  pairs that are plainly unrelated — are not taken, so as to measure the
  plain version first.
- **A query does not resume.** Each stage's files are written as it finishes,
  but a run that dies in the reduction starts the reduction over. The cheap
  half is there — `/answer` in the chat puts a new question to a saved
  `points.md` in one request — but a saved `points.raw.md` cannot yet be
  handed back to the reduction.
- **One model for every role.** The mask and the comparisons could go to a
  smaller model than the reads; there is one `--model` for now.
- **Documents that do not fit are skipped.** Deliberately, for now.
