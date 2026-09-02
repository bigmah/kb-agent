//! Drawing the OCR page counter, which is the only sign of life during a run
//! that can last a quarter of an hour.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;

use pdf_extractor::Progress;

/// Pages between lines when stderr is not a terminal.
const MILESTONE: usize = 25;

/// A progress callback for [`Options::progress`](pdf_extractor::Options::progress).
///
/// On a terminal it redraws one line in place; piped, it prints occasionally,
/// because a redrawn line in a log file is just noise.
pub fn terminal() -> impl Fn(Progress) + Send + Sync + 'static {
    let state = Mutex::new(Counter {
        interactive: std::io::stderr().is_terminal(),
        last: 0,
        milestone: 0,
    });
    move |event| {
        // A poisoned lock would mean a panic inside this closure; there is
        // nothing here that can panic, and losing the counter is not worth
        // taking the conversion down for.
        let Ok(mut counter) = state.lock() else {
            return;
        };
        counter.on(event);
    }
}

struct Counter {
    interactive: bool,
    last: usize,
    milestone: usize,
}

impl Counter {
    fn on(&mut self, event: Progress) {
        match event {
            Progress::OcrStarting { .. } => {
                self.last = 0;
                self.milestone = 0;
            }
            Progress::OcrPage { done, total } => self.page(done, total),
            Progress::OcrFinished => self.finish(),
            // `Progress` is non-exhaustive: a new stage this build has never
            // heard of is not a reason to stop counting pages.
            _ => {}
        }
    }

    fn page(&mut self, done: usize, total: usize) {
        if total == 0 || done == self.last {
            return;
        }
        self.last = done;
        if self.interactive {
            eprint!("\rOCR: page {done} of {total}");
            let _ = std::io::stderr().flush();
        } else if done / MILESTONE > self.milestone {
            // Workers finish in bursts, so the count can step over an exact
            // multiple; report on crossing one rather than on landing on it.
            self.milestone = done / MILESTONE;
            eprintln!("OCR: page {done} of {total}");
        }
    }

    fn finish(&mut self) {
        if self.interactive && self.last > 0 {
            // Erase the counter so it does not sit above the summary.
            eprint!("\r{:width$}\r", "", width = 32);
            let _ = std::io::stderr().flush();
        }
    }
}
