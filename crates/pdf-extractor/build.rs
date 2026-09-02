//! Fetches the two shared libraries the OCR path loads, if they are not
//! already somewhere the binary can find them.
//!
//! PDFium and ONNX Runtime are `dlopen`ed at runtime rather than linked, so
//! nothing in the ordinary build graph pulls them in — but this crate OCRs
//! whenever a PDF needs it, which means a build that skipped this step fails
//! later, on a scanned document, rather than here. Provisioning them at build
//! time keeps `cargo build --release` the only command anyone has to run.
//!
//! This never fails the build. A machine that is offline, or that already has
//! the libraries installed system-wide in a location this file does not know
//! about, still gets a working build for text PDFs, and `src/convert.rs`
//! explains the miss if OCR is ever actually needed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=scripts/fetch-ocr-runtime.sh");
    println!("cargo::rerun-if-env-changed=PDF_EXTRACTOR_SKIP_VENDOR");

    if std::env::var_os("PDF_EXTRACTOR_SKIP_VENDOR").is_some_and(|value| !value.is_empty()) {
        return;
    }
    if cfg!(target_os = "windows") {
        // scripts/fetch-ocr-runtime.sh is a POSIX shell script and the release
        // archive layouts differ; a Windows build wants the libraries placed
        // by hand or by a package manager.
        return;
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if already_present(&root) {
        return;
    }

    let script = root.join("scripts").join("fetch-ocr-runtime.sh");
    if !script.is_file() {
        warn(&format!("{} is missing; skipping", script.display()));
        return;
    }

    println!("cargo::warning=fetching the PDFium and ONNX Runtime libraries used for OCR");
    match Command::new("bash").arg(&script).status() {
        Ok(status) if status.success() => {}
        Ok(status) => warn(&format!(
            "fetch-ocr-runtime.sh exited with {status}; OCR will not work until it succeeds"
        )),
        Err(error) => warn(&format!("could not run fetch-ocr-runtime.sh: {error}")),
    }
}

/// Whether both libraries are already reachable.
///
/// This deliberately mirrors, in miniature, the search in `src/runtime.rs`:
/// the vendor tree this script populates plus the usual package-manager
/// prefixes. It only has to be right enough to avoid a pointless download.
fn already_present(root: &Path) -> bool {
    let extension = if cfg!(any(target_os = "macos", target_os = "ios")) {
        "dylib"
    } else {
        "so"
    };
    let directories = [
        root.join("vendor").join("pdfium").join("lib"),
        root.join("vendor").join("onnxruntime").join("lib"),
        PathBuf::from("/opt/homebrew/lib"),
        PathBuf::from("/usr/local/lib"),
        PathBuf::from("/usr/lib"),
    ];
    ["pdfium", "onnxruntime"].iter().all(|stem| {
        directories.iter().any(|directory| {
            directory.join(format!("lib{stem}.{extension}")).is_file()
                || versioned(directory, stem, extension)
        })
    })
}

/// Matches `libonnxruntime.1.29.0.dylib`, which some archives ship instead of
/// (or alongside) the unversioned name.
fn versioned(directory: &Path, stem: &str, extension: &str) -> bool {
    let prefix = format!("lib{stem}.");
    let suffix = format!(".{extension}");
    std::fs::read_dir(directory).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(&suffix))
        })
    })
}

fn warn(message: &str) {
    println!("cargo::warning={message}");
}
