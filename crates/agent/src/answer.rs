//! The answering role: the question, and what the library had to say about it.
//!
//! This is the last request, and the only one that sees the whole of what the
//! reading produced. It is told to answer from the points and to say which
//! sources each part of the answer rests on, so that whoever asked can go and
//! read the book.

use std::path::Path;

use crate::{Error, Options, Reply};

const PREAMBLE: &str = "\
You answer a question from a list of points distilled from a library of books \
and papers. Each point ends with the sources it came from, in brackets. The \
points are all you have: they are what the library says, and you are answering \
on the library's behalf.

Requirements:
- Answer the question directly, in Markdown, as a knowledgeable colleague \
would: lead with the answer, then the reasoning and the evidence.
- Rest every claim on the points. Name the sources a claim rests on, in \
brackets after it, using the names the points use. Where the points disagree, \
say so and say who says what — do not average them into a consensus that no \
source holds.
- Where the points do not cover part of the question, say that the library is \
silent on it rather than filling the gap from elsewhere.
- Prefer the specific, the quantified and the mechanistic over the general. \
Keep the authors' terminology, figures and dates exactly.
- Do not praise the question, do not hedge for its own sake, and do not \
summarize the points back — use them.
- Return only the answer. No preamble, no sign-off, no code fence around it.";

impl Options {
    /// Answer `question` from `points`, a Markdown list where each bullet ends
    /// with its sources in brackets — the form
    /// [`kb`](https://docs.rs/kb)'s distillation writes.
    ///
    /// One request with a fresh context.
    pub async fn answer(&self, question: &str, points: &str) -> Result<Reply<String>, Error> {
        let prompt = format!("Question:\n{question}\n\nWhat the library says:\n{points}");
        self.complete("answer", PREAMBLE, &prompt, Path::new("<points>"))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_stays_inside_the_library() {
        assert!(PREAMBLE.contains("the library is silent"));
        assert!(PREAMBLE.contains("Name the sources"));
    }
}
