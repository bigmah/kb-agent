# agent

Summarizes a Markdown document with an LLM, using
[rig](https://docs.rs/rig-core). ChatGPT by default; the source document is
opened read-only and never modified.

```rust
let summary = agent::summarize_markdown_file("book.md").await?;   // writes book_summary.md
```

The output is the input's name with `_summary.md` on the end — `book.md`
becomes `book_summary.md`, `notes.txt` becomes `notes_summary.md`. Use
`Options::summarize_to_file` to put it somewhere else, or `summarize_markdown`
to keep it in memory.

## Long documents

A document that fits in one request gets one request. A longer one is split
into sections, each section is summarized, and the section summaries are
summarized in turn — so the result covers the whole document rather than its
first few pages.

**Nothing is ever truncated to fit.** That is the one rule the splitting exists
to enforce: cutting at the context limit would produce a summary of chapter one
labelled as a summary of the book, and nothing downstream could tell. Splits are
taken at the best boundary available — a Markdown heading, then a blank line,
then a line — and a fenced code block is never split down the middle.

If even the section summaries overflow one request, they are fused in groups and
the group summaries fused again, for as many rounds as it takes.

## Knowing what it will cost first

Every request costs money, and a long document is not obviously a long document
from its name. `plan` reads and splits the file without sending anything:

```rust
let plan = Options::new().plan("book.md")?;
eprintln!("{}", plan.describe());
// book.md: ~533000 tokens, 6 sections, 7 requests to gpt-5.6
```

Token counts in a `Plan` are estimated from character counts — the real number
needs the model's tokenizer, and asking the provider would cost a round trip per
candidate split. The estimate is deliberately pessimistic; see
`CHARS_PER_TOKEN`.

## Options

```rust
use agent::{Options, Progress, Provider};

let summary = Options::new()
    .provider(Provider::Anthropic)      // default is Provider::OpenAi
    .model("claude-opus-5")             // default is the provider's own
    .max_tokens(16_384)                 // output cap per request
    .section_tokens(100_000)            // source tokens per request
    .concurrency(4)                     // sections summarized at once
    .focus("Keep every figure and date. Skip the front matter.")
    .progress(|event| {
        if let Progress::Section { done, total } = event {
            eprintln!("section {done} of {total}");
        }
    })
    .summarize_to_file("book.md", "book_summary.md")
    .await?;

eprintln!("{}", summary.describe());
// done: 6 sections in 7 requests to gpt-5.6 — 541203 in, 18422 out, 4 min 2 s
```

`Summary` carries the provider's own token counts, not the estimate — what you
were actually billed for.

Errors are an enum rather than a sentence to grep. `Error::NoApiKey` names the
variable to set and is raised before a single request goes out; `Error::Empty`
is a document with nothing in it; `Error::Provider` carries the service's own
explanation.

## Providers

| | Default model | Key |
| --- | --- | --- |
| `Provider::OpenAi` (default) | `gpt-5.6` | `OPENAI_API_KEY` |
| `Provider::Anthropic` | `claude-opus-5` | `ANTHROPIC_API_KEY` |

Setting a provider also moves the default model, so the two cannot fall out of
step. Everything past the provider choice is generic over rig's
`CompletionModel`, so the provider is named in exactly one `match`.

One sharp edge worth knowing if you use rig directly: on the Anthropic path rig
derives `max_tokens` from the model name and falls back to **2048** for any name
it does not recognize — which is every model newer than the rig in your tree.
A summary would come back cut off mid-sentence with nothing in the response
saying so. This crate always sends an explicit cap; `Provider::truncates_unknown_models`
is the flag for anyone building requests themselves.

## If you are not already async

The `blocking` feature (on by default) mirrors the whole API synchronously, so a
command-line program does not have to become async to call this:

```rust
let summary = agent::blocking::summarize_markdown_file("book.md")?;
```

Each call owns a single-threaded runtime for its own duration — the right trade
for one document then exit, and the wrong one for a server, which should use the
async API and its existing runtime. Calling these from *inside* a runtime panics,
as starting a runtime inside a runtime always does.

`examples/summarize.rs` is the smallest complete program: it prints the plan,
then does the work.
