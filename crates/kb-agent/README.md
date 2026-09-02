# kb-agent

The command. The work lives in the libraries — [`pdf-extractor`](../pdf-extractor)
converts, [`agent`](../agent) makes the requests, [`kb`](../kb) decides which
document goes to which request. Everything here is what a library should not
decide: what the flags are called, where output goes, and what gets said on
the way.

```
kb-agent build <dir> [OPTIONS]                Every PDF into Markdown, every Markdown into a summary
kb-agent status <dir>                         What is built and what is not
kb-agent query <dir> "<question>" [OPTIONS]   Put a question to every document in the library
kb-agent convert <input.pdf> [OPTIONS]        One PDF into Markdown
kb-agent <input.pdf> [OPTIONS]                The same as convert
```

`kb-agent <command> --help` has every flag. The ones that make requests share
a set:

```
    --provider <name>       openai (default) or anthropic; the key comes from
                            OPENAI_API_KEY or ANTHROPIC_API_KEY
    --model <name>          Model to use (default: the provider's own)
    --concurrency <n>       Requests in flight at once (default: 8)
    --context-tokens <n>    Input budget per request; a document over it is
                            left out rather than cut down (default: 700000)
    --max-tokens <n>        Output cap per request (default: 50000)
    --retries <n>           Resends when the provider is busy (default: 5)
```

## build

```
$ kb-agent build library/
library/: 12 sources: 0 sources ready to query, 12 PDFs to convert, 0 documents to summarize
build: 12 to convert, 12 to summarize with gpt-5.6 via OpenAI, 8 at a time
convert: finance/johnson-algorithmic-trading (1 of 12)
finance/johnson-algorithmic-trading.pdf: 595 page(s), Scanned (confidence 0.95)
OCR: 595 page(s) at 300 DPI on CPU across 7 workers, about 2 min
...
convert: 12 PDFs converted, 3 min 41 s
summarize: 12 of 12
summarize: 11 documents summarized, 1 too large — 11 requests, 4210553 in, 61204 out, 6 min 2 s
  finance/hull-options-futures: ~912000 tokens, over the 700000-token budget — raise --context-tokens or split the document
library/: 12 sources: 11 sources ready to query, 1 document to summarize
```

Both steps skip what is already there, so a build picks up where it left off
and a document dropped in later is picked up by the next one. `--plan` says
what would be done and stops; `--force` redoes summaries; `--reconvert` redoes
Markdown; `--no-summaries` converts without sending anything. The conversion
flags that matter for a batch — `--ocr`, `--ocr-dpi`, `--ocr-jobs`,
`--compact` — are accepted here too.

A PDF that cannot be converted or a document whose request fails after its
retries is reported and the rest carry on; the exit code says whether anything
was left behind.

## query

```
$ kb-agent query library/ "What limits the throughput of an order book?"
library/: 11 sources to judge, 1 excluded, with gpt-5.6 via OpenAI, 8 at a time
  excluded finance/hull-options-futures: not summarized yet
writing to library/.kb-agent/queries/20260902-141500-what-limits-the-throughput-of-an-order-book
mask: 11 of 11
mask: 4 of 11 sources relevant (1 excluded) — 11 requests, 118204 in, 22 out, 8.1 s
read: 4 of 4
read: 47 points from 4 sources — 4 requests, 1830112 in, 6210 out, 2 min 40 s
reduce: 1081 pairs to compare across 47 points
compare: 1081 of 1081
merge: 9 of 9
reduce: 1081 pairs compared, 47 → 33 points (9 merged) — compare 1081 requests, 302680 in, 1081 out, 1 min 12 s; merge 9 requests, 3105 in, 1220 out, 6.4 s
answer: writing
answer: 1 request, 4102 in, 1380 out, 21.0 s
total: 1106 requests, 2258203 in, 9913 out, 4 min 28 s
wrote library/.kb-agent/queries/20260902-141500-what-limits-the-throughput-of-an-order-book
```

The answer goes to stdout; everything else goes to stderr, so the answer
pipes. The directory holds each stage's output, written the moment that stage
finishes, so a run that dies in the reduction has already saved the list it
was reducing:

| File | What |
| --- | --- |
| `question.md` | the question |
| `mask.md` | which sources were judged relevant, which were not, which were excluded and why |
| `points.raw.md` | every point every relevant document made, each with its source |
| `points.md` | the same after reduction — the list the answer was written from |
| `answer.md` | the answer |
| `report.md` | the cost of each stage |

`points.md` is the thing to carry into another agent's context when the
question was a step in something larger: one bullet per thing the library
says, each ending with the sources that say it.

`--plan` says which sources would be judged and stops. `--no-reduce` skips the
pairwise stage, which costs the square of the list — the count is printed
before it starts, so there is time to reconsider. `--no-answer` stops at the
list and prints that instead. `-o <dir>` puts the files somewhere other than
under the library.

A request that fails after its retries stops the query: the answer comes from
the whole library or not at all.

## convert

```
$ kb-agent convert samples/book.pdf
samples/book.pdf: 595 page(s), Scanned (confidence 0.95)
OCR: 595 page(s) at 300 DPI on CPU across 7 workers, about 2 min — the ~31 MB model set downloads on first use
OCR: page 300 of 595
done: 595 page(s), 595 OCR'd — render 53.5 s, recognize 24 min 9 s across 7 workers, total 3 min 41 s
wrote samples/book.md (1620659 bytes)
```

Markdown goes to the file; a plan line, a summary line, and any warnings go to
stderr, so `--stdout` stays pipeable. The program refuses to write over its own
input, and checks that before starting rather than after. Recognition time is
summed across workers, so on a parallel run it is larger than the wall-clock
total rather than smaller. The
[library README](../pdf-extractor/README.md#ocr) explains why the run is shaped
this way, and what the DPI and job count actually cost.

## Being a worker

`main` gives the extractor first refusal on the process:

```rust
if let Some(code) = pdf_extractor::run_worker_if_spawned() {
    return code;
}
```

An OCR worker is this same executable, re-invoked with no arguments, so that
line has to come before the command line is parsed. Without it there is no
fan-out — see [`run_worker_if_spawned`](../pdf-extractor/README.md#being-a-host).
