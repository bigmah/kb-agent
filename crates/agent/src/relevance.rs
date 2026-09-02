//! The masking role: is this document worth reading for this question?
//!
//! It sees the summary, not the document, and it is asked once per document
//! per question with nothing else in its context — so its answer about one
//! document cannot be coloured by the ninety-nine it saw before.

use std::path::Path;

use crate::{Error, Options, Reply};

const PREAMBLE: &str = "\
You decide whether a document is worth reading in full to answer a question. \
You are given the question and a summary of the document — not the document \
itself.

Answer YES if the document plausibly contains information, evidence, methods, \
examples, arguments or counter-arguments that would help answer the question, \
even partially or indirectly. Answer NO only if it clearly would not. When \
unsure, answer YES: an unnecessary read costs a little, a missed one costs the \
answer.

Reply with exactly one word: YES or NO.";

impl Options {
    /// Whether a document, as described by `summary`, could help answer
    /// `question`.
    ///
    /// One request with a fresh context. Leans towards `true`: the read it
    /// gates is cheap next to the answer it would otherwise be missing from.
    pub async fn relevant(&self, question: &str, summary: &str) -> Result<Reply<bool>, Error> {
        let prompt = format!("Question:\n{question}\n\nSummary of the document:\n{summary}");
        self.complete("relevance", PREAMBLE, &prompt, Path::new("<summary>"))
            .await?
            .try_map(|text| parse_verdict(&text, "YES", "NO", "relevance"))
    }
}

/// Read a one-word verdict out of a reply, leniently: the first word in the
/// text that is one of the two allowed, whatever punctuation or preamble the
/// model wrapped it in.
pub(crate) fn parse_verdict(
    text: &str,
    yes: &str,
    no: &str,
    role: &'static str,
) -> Result<bool, Error> {
    for word in text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
    {
        if word.eq_ignore_ascii_case(yes) {
            return Ok(true);
        }
        if word.eq_ignore_ascii_case(no) {
            return Ok(false);
        }
    }
    Err(Error::Unparseable {
        role,
        text: text.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_is_read_through_punctuation_and_case() {
        let read = |text| parse_verdict(text, "YES", "NO", "relevance").unwrap();
        assert!(read("YES"));
        assert!(read("Yes."));
        assert!(read("**YES** — the chapter on order books applies."));
        assert!(!read("no"));
        assert!(!read("NO, nothing here."));
        // The first allowed word wins, so a hedge that lands on one is read as
        // that one rather than as noise.
        assert!(!read("Answer: NO. (It would be YES for a different question.)"));
    }

    #[test]
    fn a_reply_with_neither_word_is_an_error_that_keeps_the_text() {
        let error = parse_verdict("Maybe?", "YES", "NO", "relevance").expect_err("unreadable");
        assert!(
            matches!(&error, Error::Unparseable { role: "relevance", text } if text == "Maybe?"),
            "{error:?}"
        );
    }

    #[test]
    fn the_preamble_leans_towards_reading() {
        assert!(PREAMBLE.contains("When unsure, answer YES"));
    }
}
