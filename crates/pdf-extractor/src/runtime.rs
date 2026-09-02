//! Locating the two shared libraries the OCR path opens at runtime.
//!
//! `pdf-inspector` links neither PDFium nor ONNX Runtime. It `dlopen`s both the
//! first time a page actually needs OCR, and finds them through the
//! `PDFIUM_LIB_PATH` and `ORT_DYLIB_PATH` environment variables or the system
//! loader's search path. Making the caller export those first is exactly the
//! kind of setup this library exists to absorb, so it looks for the libraries
//! itself and fills the variables in before the OCR pipeline reads them.
//!
//! Nothing here loads a library or fails: a variable that cannot be filled in
//! is simply left alone, and the OCR path reports the miss if and when it
//! needs the library.

use std::path::{Path, PathBuf};

/// Environment variable naming the PDFium shared library.
const PDFIUM_ENV: &str = "PDFIUM_LIB_PATH";
/// Environment variable naming the ONNX Runtime shared library.
const ORT_ENV: &str = "ORT_DYLIB_PATH";

#[cfg(target_os = "windows")]
const LIB_EXTENSION: &str = "dll";
#[cfg(any(target_os = "macos", target_os = "ios"))]
const LIB_EXTENSION: &str = "dylib";
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "ios")))]
const LIB_EXTENSION: &str = "so";

#[cfg(target_os = "windows")]
const LIB_PREFIX: &str = "";
#[cfg(not(target_os = "windows"))]
const LIB_PREFIX: &str = "lib";

/// Points `PDFIUM_LIB_PATH` and `ORT_DYLIB_PATH` at the libraries this build
/// can find, unless the environment already names them.
///
/// Idempotent, and a no-op once either variable is set — including by a
/// caller who would rather point at a system-wide install.
///
/// # Call this from `main`
///
/// This writes process environment variables, which is only sound while the
/// process is single-threaded. Every conversion calls it, so a program that
/// only ever converts need not; but a program that spawns threads for anything
/// else first must call it before it does, or the write races whatever those
/// threads are reading.
pub fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(prepare);
}

fn prepare() {
    for (variable, library) in [
        (PDFIUM_ENV, Library::Pdfium),
        (ORT_ENV, Library::OnnxRuntime),
    ] {
        // An explicit setting is the operator's business, not ours. Honour an
        // empty value too: `pdf-inspector` treats that as "unset".
        if std::env::var_os(variable).is_some_and(|value| !value.is_empty()) {
            continue;
        }
        let Some(found) = find(library) else { continue };
        // SAFETY: documented above — single-threaded at the point of call.
        unsafe { std::env::set_var(variable, found) };
    }
}

/// A description of where the OCR libraries were looked for, for error text.
pub(crate) fn searched_locations() -> Vec<String> {
    search_directories()
        .iter()
        .map(|directory| directory.display().to_string())
        .collect()
}

#[derive(Clone, Copy)]
enum Library {
    Pdfium,
    OnnxRuntime,
}

fn find(library: Library) -> Option<PathBuf> {
    search_directories()
        .iter()
        .find_map(|directory| match library {
            Library::Pdfium => exact(directory, "pdfium"),
            // ONNX Runtime ships both `libonnxruntime.dylib` and a versioned
            // `libonnxruntime.1.29.0.dylib`; either loads, so take whichever
            // the release archive actually left behind.
            Library::OnnxRuntime => {
                exact(directory, "onnxruntime").or_else(|| versioned(directory, "onnxruntime"))
            }
        })
}

fn exact(directory: &Path, stem: &str) -> Option<PathBuf> {
    let candidate = directory.join(format!("{LIB_PREFIX}{stem}.{LIB_EXTENSION}"));
    candidate.is_file().then_some(candidate)
}

/// Matches `libonnxruntime.<version>.dylib` and friends, newest name last so
/// the highest version wins a lexical tie-break.
fn versioned(directory: &Path, stem: &str) -> Option<PathBuf> {
    let prefix = format!("{LIB_PREFIX}{stem}.");
    let suffix = format!(".{LIB_EXTENSION}");
    let mut matches: Vec<PathBuf> = std::fs::read_dir(directory)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(&suffix))
        })
        .collect();
    matches.sort();
    matches.pop()
}

/// Directories probed for both libraries, in priority order.
fn search_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();

    // The vendor tree next to the sources this binary was built from. This is
    // what `scripts/fetch-ocr-runtime.sh` (and `build.rs`) populate, and it
    // keeps working after the binary is copied somewhere else.
    push_vendor(&mut directories, Path::new(env!("CARGO_MANIFEST_DIR")));

    // A relocated binary with its libraries alongside, plus the vendor tree of
    // whatever tree it sits in — `target/release/kb-agent` reaches the workspace
    // root in two hops. Inside this workspace `CARGO_MANIFEST_DIR` above is the
    // one that actually hits; this covers a binary that was moved.
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        directories.push(directory.to_path_buf());
        for ancestor in directory.ancestors().take(4) {
            push_vendor(&mut directories, ancestor);
        }
    }

    // Package-managed installs: Homebrew on either architecture, then the
    // ordinary Unix prefixes.
    for prefix in ["/opt/homebrew", "/usr/local", "/usr"] {
        directories.push(Path::new(prefix).join("lib"));
    }

    // The executable's ancestors usually reach the same tree as
    // CARGO_MANIFEST_DIR, so the same directory arrives more than once and not
    // always consecutively.
    let mut seen = std::collections::HashSet::new();
    directories.retain(|directory| seen.insert(directory.clone()));
    directories
}

fn push_vendor(directories: &mut Vec<PathBuf>, root: &Path) {
    directories.push(root.join("vendor").join("pdfium").join("lib"));
    directories.push(root.join("vendor").join("onnxruntime").join("lib"));
}
