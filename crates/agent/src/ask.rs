//! The source role: answer a question from one document and nothing else.
//!
//! This is the reader in the reading pool. It is handed the whole document and
//! the question, with nothing else in its context, and it returns what that
//! document — only that document — has to say, as a list of self-contained
//! points. It is told not to bring in what it knows from elsewhere, because
//! the whole point of reading the library is to learn what the library says.

use std::path::Path;

use crate::{Error, Options, Reply};

const PREAMBLE: &str = "\
You answer a question using one document and nothing else. You are given the \
question and the full text of the document.

Requirements:
- Use only what the document says. Do not bring in outside knowledge, and do \
not fill gaps with what is usually true. If the document has nothing that bears \
on the question, reply with the single word NONE.
- Return a Markdown bullet list, one point per bullet. A point is a fact, \
finding, method, mechanism, example, figure, argument or caveat from the \
document that bears on the question, stated so that it makes sense on its own \
— without the other bullets and without the document.
- Preserve the author's terminology, names, figures and dates exactly. Where a \
point rests on evidence the document gives, say what the evidence is.
- Include what the document says even where it argues against the premise of \
the question, and note where it disagrees with itself.
- Prefer specific over general. Do not pad: as many points as the document \
supports and no more, and no bullet that restates another.
- Return only the list. No heading, no preamble, no sign-off, no code fence.";

impl Options {
    /// What `source` has to say about `question`, as self-contained points.
    ///
    /// One request with a fresh context; `label` is what the source is called
    /// in a refusal, which is its path where it has one. An empty list means
    /// the document had nothing to offer, which is a valid answer.
    pub async fn ask(
        &self,
        question: &str,
        source: &str,
        label: impl AsRef<Path>,
    ) -> Result<Reply<Vec<String>>, Error> {
        let prompt = format!("Question:\n{question}\n\nDocument:\n{source}");
        Ok(self
            .complete("source", PREAMBLE, &prompt, label.as_ref())
            .await?
            .map(|text| parse_points(&text)))
    }
}

/// Read a bullet list out of a reply, without losing anything the model put
/// outside the bullets.
///
/// A line that starts with a list marker starts a point; indented or unmarked
/// lines that follow it are its continuation. A bare paragraph with no marker
/// is a point of its own rather than noise, because a model that ignored the
/// format still said something. Headings and the word `NONE` are dropped.
pub(crate) fn parse_points(text: &str) -> Vec<String> {
    let mut points: Vec<String> = Vec::new();
    let mut open = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            open = false;
            continue;
        }
        if line.starts_with('#') || line.eq_ignore_ascii_case("none") {
            open = false;
            continue;
        }
        match strip_marker(line) {
            Some(rest) => {
                points.push(rest.trim().to_string());
                open = true;
            }
            None if open && raw.starts_with([' ', '\t']) => {
                if let Some(last) = points.last_mut() {
                    last.push(' ');
                    last.push_str(line);
                }
            }
            None => {
                points.push(line.to_string());
                open = true;
            }
        }
    }
    points.retain(|point| !point.is_empty());
    points
}

/// The text after a Markdown list marker, if the line has one. A marker with
/// nothing after it is an empty bullet, which the caller drops.
fn strip_marker(line: &str) -> Option<&str> {
    for marker in ["-", "*", "+", "•", "–", "—"] {
        if line == marker {
            return Some("");
        }
        if let Some(rest) = line.strip_prefix(marker)
            && rest.starts_with(' ')
        {
            return Some(rest);
        }
    }
    // `1. ` and `1) `
    let digits = line.trim_start_matches(|c: char| c.is_ascii_digit());
    if digits.len() < line.len()
        && let Some(rest) = digits.strip_prefix(". ").or_else(|| digits.strip_prefix(") "))
    {
        return Some(rest);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bullets_of_every_common_shape_are_read() {
        let text = "- first\n* second\n+ third\n• fourth\n1. fifth\n2) sixth\n";
        assert_eq!(
            parse_points(text),
            ["first", "second", "third", "fourth", "fifth", "sixth"]
        );
    }

    #[test]
    fn a_wrapped_bullet_is_one_point() {
        let text = "- The spread is wider at the open,\n  and narrows through the day.\n- Second.";
        assert_eq!(
            parse_points(text),
            [
                "The spread is wider at the open, and narrows through the day.",
                "Second."
            ]
        );
    }

    #[test]
    fn none_headings_and_blank_lines_are_not_points() {
        assert!(parse_points("NONE").is_empty());
        assert!(parse_points("none\n").is_empty());
        assert_eq!(parse_points("## Points\n\n- one\n\n\n- two\n"), ["one", "two"]);
        assert_eq!(parse_points("- \n- real\n"), ["real"]);
    }

    #[test]
    fn a_bare_paragraph_is_kept_rather_than_lost() {
        // The model ignored the format; what it said still counts.
        assert_eq!(
            parse_points("The book covers only equities.\n\n- and this"),
            ["The book covers only equities.", "and this"]
        );
    }

    #[test]
    fn the_preamble_keeps_the_reader_inside_the_document() {
        assert!(PREAMBLE.contains("Do not bring in outside knowledge"));
        assert!(PREAMBLE.contains("NONE"));
    }
}
