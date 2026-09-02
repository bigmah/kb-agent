//! Drawing the counters, which are the only sign of life during a run that
//! can last a quarter of an hour.
//!
//! On a terminal each counter redraws one line in place; piped, it prints
//! occasionally, because a redrawn line in a log file is just noise.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;

/// Units between lines when stderr is not a terminal.
const MILESTONE: usize = 25;

/// A progress callback for [`pdf_extractor::Options::progress`].
pub fn ocr() -> impl Fn(pdf_extractor::Progress) + Send + Sync + 'static {
    let counter = Mutex::new(Counter::new("OCR: page"));
    move |event| {
        // A poisoned lock would mean a panic inside this closure; there is
        // nothing here that can panic, and losing the counter is not worth
        // taking the conversion down for.
        let Ok(mut counter) = counter.lock() else {
            return;
        };
        match event {
            pdf_extractor::Progress::OcrStarting { .. } => counter.reset("OCR: page"),
            pdf_extractor::Progress::OcrPage { done, total } => counter.count(done, total),
            pdf_extractor::Progress::OcrFinished => counter.finish(),
            // `Progress` is non-exhaustive: a new stage this build has never
            // heard of is not a reason to stop counting pages.
            _ => {}
        }
    }
}

/// A progress callback for the `progress` on [`kb`]'s option types: one
/// counter, relabelled as each stage begins.
pub fn stages() -> impl Fn(kb::Progress) + Send + Sync + 'static {
    let counter = Mutex::new(Counter::new(""));
    move |event| {
        let Ok(mut counter) = counter.lock() else {
            return;
        };
        use kb::Progress as P;
        match event {
            P::Converting { name, done, total } => {
                counter.finish();
                eprintln!("convert: {name} ({} of {total})", done + 1);
            }
            P::Summarizing { done, total } => counter.stage("summarize", done, total),
            P::Masking { done, total } => counter.stage("mask", done, total),
            P::Asking { done, total } => counter.stage("read", done, total),
            P::Comparing { done, total } => counter.stage("compare", done, total),
            P::Merging { done, total } => counter.stage("merge", done, total),
            P::Answering => {
                counter.finish();
                eprintln!("answer: writing");
            }
            _ => {}
        }
    }
}

struct Counter {
    interactive: bool,
    label: &'static str,
    last: usize,
    milestone: usize,
    drawn: bool,
}

impl Counter {
    fn new(label: &'static str) -> Self {
        Self {
            interactive: std::io::stderr().is_terminal(),
            label,
            last: 0,
            milestone: 0,
            drawn: false,
        }
    }

    fn reset(&mut self, label: &'static str) {
        self.finish();
        self.label = label;
        self.last = 0;
        self.milestone = 0;
    }

    /// Count under `label`, starting a new counter if the label changed.
    fn stage(&mut self, label: &'static str, done: usize, total: usize) {
        if label != self.label {
            self.reset(label);
        }
        self.count(done, total);
        if done == total {
            self.finish();
            eprintln!("{label}: {done} of {total}");
        }
    }

    fn count(&mut self, done: usize, total: usize) {
        if total == 0 || done == self.last {
            return;
        }
        self.last = done;
        if self.interactive {
            eprint!("\r{}: {done} of {total}", self.label);
            let _ = std::io::stderr().flush();
            self.drawn = true;
        } else if done / MILESTONE > self.milestone {
            // Workers finish in bursts, so the count can step over an exact
            // multiple; report on crossing one rather than on landing on it.
            self.milestone = done / MILESTONE;
            eprintln!("{}: {done} of {total}", self.label);
        }
    }

    fn finish(&mut self) {
        if self.interactive && self.drawn {
            // Erase the counter so it does not sit above the next line.
            eprint!("\r{:width$}\r", "", width = 48);
            let _ = std::io::stderr().flush();
            self.drawn = false;
        }
    }
}
