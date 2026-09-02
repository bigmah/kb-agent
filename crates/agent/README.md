# agent

One LLM request with a fresh context, in each of the roles a knowledge base
needs, using [rig](https://docs.rs/rig-core). ChatGPT by default; nothing here
is ever written to disk except by the two functions whose names say so.

```rust
let summary = agent::summarize_markdown_file("book.md").await?;   // writes book_summary.md
```

Every role is the same shape: some material goes out under instructions for
that role, in one request, into a context holding nothing else, and the
model's text comes back read into whatever the role returns. The roles are
methods on `Options`:

| Role | Sees | Returns |
| --- | --- | --- |
| `summarize` | one document | a shorter document |
| `relevant` | a question and one summary | whether the document is worth reading |
| `ask` | a question and one document | what that document says, as a list of points |
| `same_point` | two points | whether they carry the same information |
| `merge_points` | points judged the same | the one point that carries all of them |
| `answer` | a question and the distilled points | the answer, with sources named |

Each returns a `Reply<T>`: the value, the model, and the provider's own token
counts as a `Usage` — which adds up, so a run of ten thousand requests reports
itself with the same type one request does.

The prompts are the roles. `ask` is told to use the document and nothing else
and to say `NONE` when it has nothing; `relevant` is told to lean towards yes,
since an unnecessary read is cheap and a missed one costs the answer;
`same_point` is told that wording, precision and harmless qualifiers do not
make two points different, because the merge that follows keeps every
qualifier and so a false "same" costs almost nothing; `answer` is told to name its
sources and to say where the library is silent rather than fill the gap.

## One input, one request

The material is sent whole or not at all. There is no splitting and no
truncating: an input over the context budget is refused with
`Error::TooLarge`, naming its size and the budget it missed.

That is deliberate, and it is the point of the crate. A summary of part of a
document reads exactly like a summary of all of it, and an answer from part of
a document is worse — nothing downstream can tell a reading of the book from a
reading of chapter one — so a document that does not fit is a problem for
whatever produced it, not something to paper over here. Convert fewer pages,
raise `context_tokens` if the model's window has room, or split the document
on purpose.

## Knowing what it will cost first

The request costs money, and a document is not obviously an oversized document
from its name. `plan` reads and measures the file without sending anything, so
the refusal above costs nothing to discover; `plan_text` does the same for
text already in memory.

```rust
let plan = Options::new().plan("book.md")?;
eprintln!("{}", plan.describe());
// book.md: ~533000 tokens, 1 request to gpt-5.6
// book.md: ~2100000 tokens, over the 700000-token budget — will be refused
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
    .max_tokens(16_384)                 // output cap per request
    .context_tokens(700_000)            // input tokens a request may carry
    .retries(5)                         // resends when the provider is busy
    .focus("Keep every figure and date.")   // summaries only
    .progress(|event| {
        if let Progress::Retrying { attempt, after_ms } = event {
            eprintln!("busy; retry {attempt} in {after_ms} ms");
        }
    })
    .summarize_to_file("book.md", "book_summary.md")
    .await?;

eprintln!("{}", summary.describe());
// done: 1 request to claude-opus-5 — 541203 in, 18422 out, 1 min 12 s
```

A provider that answers with a rate limit, an overload, a gateway error or a
dropped connection is not reporting a problem with the request, so the request
goes out again after a pause — doubling from a second, capped at thirty, with
a little jitter so a hundred refused together do not come back together. A bad
key, a bad model name or a malformed request fails at once. `retries(0)` sends
everything exactly once.

Errors are an enum rather than a sentence to grep. `Error::NoApiKey` names the
variable to set and is raised before a single request goes out; `Error::TooLarge`
carries the input's size and the budget it missed; `Error::Unparseable` is a
verdict that was neither of the words allowed, with the text the model did
send; `Error::Provider` carries the service's own explanation.

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
A reply would come back cut off mid-sentence with nothing in the response
saying so. This crate always sends an explicit cap; `Provider::truncates_unknown_models`
is the flag for anyone building requests themselves.

## If you are not already async

The `blocking` feature (on by default) mirrors the summarizing API
synchronously, so a command-line program does not have to become async to
call it:

```rust
let summary = agent::blocking::summarize_markdown_file("book.md")?;
```

Each call owns a single-threaded runtime for its own duration — the right
trade for one document then exit, and the wrong one for a server, which should
use the async API and its existing runtime. Calling these from *inside* a
runtime panics, as starting a runtime inside a runtime always does. The other
roles are async only: anything asking a question of a whole library is running
many requests at once, and already has a runtime.

`examples/summarize.rs` is the smallest complete program: it prints the plan,
then does the work.
