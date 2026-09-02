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

## One document, one request

The document is sent whole or not at all. There is no splitting and no
truncating: a document over the context budget is refused with
`Error::TooLarge`, naming its size and the budget it missed.

That is deliberate, and it is the point of the crate. A summary of part of a
document reads exactly like a summary of all of it — nothing downstream can tell
a summary of the book from a summary of chapter one — so a document that does
not fit is a problem for whatever produced it, not something to paper over here.
Convert fewer pages, raise `context_tokens` if the model's window has room, or
summarize the pieces separately and on purpose.

## Knowing what it will cost first

The request costs money, and a document is not obviously an oversized document
from its name. `plan` reads and measures the file without sending anything, so
the refusal above costs nothing to discover:

```rust
let plan = Options::new().plan("book.md")?;
eprintln!("{}", plan.describe());
// book.md: ~533000 tokens, 1 request to gpt-5.6
// book.md: ~2100000 tokens, over the 700000-token budget — will be refused

if !plan.fits {
    // fix it upstream rather than sending it
}
```

Token counts in a `Plan` are estimated from character counts — the real number
needs the model's tokenizer, and asking the provider would cost a round trip.
The estimate is deliberately pessimistic; see `CHARS_PER_TOKEN`.

## Options

```rust
use agent::{Options, Progress, Provider};

let summary = Options::new()
    .provider(Provider::Anthropic)      // default is Provider::OpenAi
    .model("claude-opus-5")             // default is the provider's own
    .max_tokens(16_384)                 // output cap for the summary
    .context_tokens(700_000)            // source tokens the request may carry
    .focus("Keep every figure and date. Skip the front matter.")
    .progress(|event| {
        if event == Progress::Requesting {
            eprintln!("sending");
        }
    })
    .summarize_to_file("book.md", "book_summary.md")
    .await?;

eprintln!("{}", summary.describe());
// done: 1 request to claude-opus-5 — 541203 in, 18422 out, 1 min 12 s
```

`Summary` carries the provider's own token counts, not the estimate — what you
were actually billed for.

Errors are an enum rather than a sentence to grep. `Error::NoApiKey` names the
variable to set and is raised before a single request goes out; `Error::TooLarge`
carries the document's size and the budget it missed; `Error::Empty` is a
document with nothing in it; `Error::Provider` carries the service's own
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
