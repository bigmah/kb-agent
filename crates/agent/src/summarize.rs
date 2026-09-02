//! The summarizing role: one document in, a shorter document out.

use std::path::Path;

use crate::{Error, Options, Reply};

/// How the model is told to behave.
const PREAMBLE: &str = "\
You summarize documents. You are given a document in Markdown and you return a \
summary of it in Markdown.

Requirements:
- Open with a single paragraph saying what the document is and what it covers.
- Then the substance, under `##` headings that follow the document's own \
structure. Preserve the author's terminology, names, figures and dates exactly.
- Record what the document actually says, including conclusions you find \
unconvincing. Do not add commentary, praise, or information from outside it.
- If part of the document is unreadable or looks garbled, say so in one line \
rather than guessing at it.
- Return only the summary. No preamble, no sign-off, no code fence around it.";

pub(crate) async fn summarize(
    text: &str,
    path: &Path,
    options: &Options,
) -> Result<Reply<String>, Error> {
    let preamble = match &options.focus {
        Some(focus) => format!("{PREAMBLE}\n\nAlso, from the person asking:\n{focus}"),
        None => PREAMBLE.to_string(),
    };
    options.complete("summary", &preamble, text, path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preamble_forbids_a_code_fence() {
        // Bare Markdown only; a fenced response would land in the output file
        // as a literal code block.
        assert!(PREAMBLE.contains("No preamble"));
        assert!(PREAMBLE.contains("code fence"));
    }
}
