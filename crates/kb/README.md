# kb

A directory of documents as a knowledge base: index it, build it, and ask it
a question that every document in it gets to answer.

```rust
use kb::{Corpus, ConvertOptions, QueryOptions, SummarizeOptions};

let mut library = Corpus::scan("library/")?;
library.convert(&ConvertOptions::new())?;              // every PDF → Markdown
library.summarize(&SummarizeOptions::new()).await?;    // every Markdown → summary

let result = library
    .query("What limits the throughput of an order book?", &QueryOptions::new())
    .await?;
result.write_to("out/")?;                              // mask, points, answer, report
```

## The shape of it

A question is not put to a model that has skimmed the library. It is put to
every document in the library, one at a time, each read in full by a request
that holds nothing else in its context. What comes back is reduced — every
pair of points compared, the repeats merged — into one list of what the
library says, and the answer is written from that list.

```
question ──► mask ──► ask ──► reduce ──► answer
              │        │        │           │
              ▼        ▼        ▼           ▼
        one request  one request  one request   one request
        per summary  per relevant per pair of   with the
                     document     points, then  reduced list
                                  one per group
```

Every stage is a fan-out of fresh-context requests — [`agent`](../agent)'s
roles — so the shape scales with the number of documents rather than with what
fits in one window. That is the bet this crate makes: that intelligence here is
a function of how much context can be brought to bear in total, not how much
fits in one place at once.

The stages are all public, because each one's output is worth keeping before
the next one is paid for:

| Stage | Method | Sees | Produces |
| --- | --- | --- | --- |
| mask | `Corpus::mask` | every summary, one per request | which sources are relevant |
| ask | `Corpus::ask` | every relevant document, in full, one per request | one long list of points, each naming its source |
| reduce | `kb::reduce` | every pair of points, one per request; then each group judged the same | the refined list |
| answer | `kb::answer` | the refined list, in one request | the answer, with sources named |

`Corpus::query` runs the four in a row and returns a `Distillation` carrying
all of it. The refined list is as much the product as the answer is: it is what
a few hundred books had to say about the question, in a form that fits in
someone else's context.

## The index is the directory

`Corpus::scan` walks the directory once, and that is the whole index. A source
is a name — its path under the root without the extension — and the three
files that may exist for it:

| File | Made by |
| --- | --- |
| `name.pdf` | whoever put it there |
| `name.md` | `Corpus::convert`, or whoever put it there |
| `name_summary.md` | `Corpus::summarize` |

There is no database to fall out of step with the files. A build that stops
half-way has lost nothing; running it again does only what is left. Adding a
document is copying a file in. Hidden files and directories are skipped, so a
query's own output can live under `.kb-agent/` without becoming part of the
library it was asked about.

`Corpus::status` says how much of the library is built, and
`Corpus::plan_query` says which sources a question can reach — a source needs a
summary to be judged and a document under the context budget to be read, and
anything short of that is excluded with the reason.

## What it costs

Every count is printed before it is spent. The mask is one small request per
summary; the read is one large request per relevant document; the answer is
one request. The reduction is the square of the list: `n` points is
`n(n-1)/2` comparisons, each tiny — `kb::pairs_for` gives the number, and
`QueryOptions::reduce(false)` turns the stage off, at the cost of an answer
written from a list that repeats itself.

Requests run `concurrency` at a time (default 8). A provider that answers with
a rate limit is retried with a backoff by `agent`, so the number is about
throughput, not safety. The first request that fails after its retries stops
the run: a question is answered from the whole library or not at all, and
half a library's worth of points would be worse than none, because nothing
downstream could tell.

Building is different — one document's failure is reported and the rest
carry on, since each summary is useful on its own and the next build picks up
the stragglers. Documents over the context budget are not summarized and are
named in the report; they stay in the library but a question cannot reach
them until they fit.

## Options

`SummarizeOptions` and `QueryOptions` each take an `agent::Options` — provider,
model, context budget, output cap, retries — plus `concurrency`, `progress`,
and for the query, `reduce` and `answer`. `ConvertOptions` takes a
`pdf_extractor::Options` for OCR and page settings, and `stop_when`, a
closure asked between PDFs so an interactive caller can end a conversion loop
on Ctrl-C without losing the document under way; the report counts what was
left. Every option has a default that works.

`Progress` is one enum for all of it: a count landed for whichever stage is
running. The `kb-agent` command's counters are one `match` on it.
