//! A directory of documents as a knowledge base: index it, build it, and ask
//! it a question that every document in it gets to answer.
//!
//! ```no_run
//! use kb::{Corpus, QueryOptions, SummarizeOptions};
//!
//! # async fn run() -> Result<(), kb::Error> {
//! let mut library = Corpus::scan("library/")?;
//! library.convert(&kb::ConvertOptions::new())?;              // every PDF → Markdown
//! library.summarize(&SummarizeOptions::new()).await?;        // every Markdown → summary
//!
//! let result = library
//!     .query("What limits the throughput of an order book?", &QueryOptions::new())
//!     .await?;
//! if let Some(answer) = &result.answer {
//!     println!("{}", answer.markdown);
//! }
//! result.write_to("library/.kb-agent/queries/order-book-throughput")?;
//! # Ok(()) }
//! ```
//!
//! # The shape of it
//!
//! A question is not put to a model that has skimmed the library; it is put
//! to every document in the library, one at a time, each read in full by a
//! request that holds nothing else in its context. What comes back is
//! reduced — every pair of points compared, the repeats merged — into one
//! list of what the library says, and the answer is written from that list.
//! Every step is a fan-out of fresh-context requests, so the shape scales
//! with the number of documents rather than with what fits in one window.
//!
//! [`Corpus`] is the index: a scan of the directory, nothing more, so there
//! is no database to fall out of step with the files. [`Corpus::convert`] and
//! [`Corpus::summarize`] build it, skipping what is already built.
//! [`Corpus::query`] runs the four stages — [`Corpus::mask`],
//! [`Corpus::ask`], [`reduce`], [`answer`] — and each is also public, so a
//! caller can keep what one stage made before paying for the next.
//!
//! The requests themselves are [`agent`]'s roles; this crate decides which
//! document goes to which role, and how many at a time.

mod build;
mod corpus;
mod error;
mod fanout;
mod query;
mod reduce;
mod report;

pub use build::{
    ConvertOptions, ConvertReport, DEFAULT_CONCURRENCY, SummarizeOptions, SummarizeReport,
};
pub use corpus::{Corpus, Source, Status};
pub use error::Error;
pub use query::{
    Answer, Distillation, Excluded, Mask, Point, QueryOptions, QueryPlan, Reading, Reduction,
    answer, pairs_for, reduce,
};
pub use report::{Progress, Stage, format_duration};
