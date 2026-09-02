//! A length-prefixed binary encoding for what crosses a process boundary.
//!
//! Two things do: the job the parent hands a worker, and the harvest the worker
//! hands back. Both carry page Markdown, which contains newlines, tabs, nulls
//! and every other plausible delimiter, so nothing line- or text-oriented can
//! frame them. This is the smallest thing that can: a magic number, then
//! fixed-width integers and length-prefixed byte strings.
//!
//! Every read is fallible, so a truncated message — a child killed mid-write —
//! reports itself rather than panicking.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(crate) struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    pub(crate) fn new(magic: &[u8; 4]) -> Self {
        let mut bytes = Vec::with_capacity(64 * 1024);
        bytes.extend_from_slice(magic);
        Self { bytes }
    }

    pub(crate) fn bool(&mut self, value: bool) -> &mut Self {
        self.bytes.push(u8::from(value));
        self
    }

    pub(crate) fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn u64(&mut self, value: u64) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    pub(crate) fn f32(&mut self, value: f32) -> &mut Self {
        self.bytes.extend_from_slice(&value.to_le_bytes());
        self
    }

    /// A count, written as the `u32` every collection here is framed by.
    pub(crate) fn len(&mut self, value: usize) -> &mut Self {
        self.u32(value as u32)
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.len(value.len());
        self.bytes.extend_from_slice(value);
        self
    }

    pub(crate) fn str(&mut self, value: &str) -> &mut Self {
        self.bytes(value.as_bytes())
    }

    pub(crate) fn opt_str(&mut self, value: Option<&str>) -> &mut Self {
        match value {
            Some(value) => self.bool(true).str(value),
            None => self.bool(false),
        }
    }

    pub(crate) fn path(&mut self, value: &Path) -> &mut Self {
        self.bytes(&encode_os(value.as_os_str()))
    }

    pub(crate) fn opt_path(&mut self, value: Option<&Path>) -> &mut Self {
        match value {
            Some(value) => self.bool(true).path(value),
            None => self.bool(false),
        }
    }

    pub(crate) fn u32s(&mut self, values: impl ExactSizeIterator<Item = u32>) -> &mut Self {
        self.len(values.len());
        for value in values {
            self.u32(value);
        }
        self
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    /// Start reading, rejecting anything that does not open with `magic`. A
    /// stale binary left in a temp directory must not be misread as a result.
    pub(crate) fn new(bytes: &'a [u8], magic: &[u8; 4]) -> Result<Self, Malformed> {
        let mut reader = Self { bytes, at: 0 };
        if reader.take(4)? != magic {
            return Err(Malformed::Unrecognized);
        }
        Ok(reader)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], Malformed> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(Malformed::Truncated)?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    pub(crate) fn bool(&mut self) -> Result<bool, Malformed> {
        Ok(self.take(1)?[0] != 0)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Malformed> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Malformed> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }

    pub(crate) fn f32(&mut self) -> Result<f32, Malformed> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    pub(crate) fn len(&mut self) -> Result<usize, Malformed> {
        Ok(self.u32()? as usize)
    }

    pub(crate) fn bytes(&mut self) -> Result<&'a [u8], Malformed> {
        let length = self.len()?;
        self.take(length)
    }

    pub(crate) fn str(&mut self) -> Result<String, Malformed> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| Malformed::NotUtf8)
    }

    pub(crate) fn opt_str(&mut self) -> Result<Option<String>, Malformed> {
        if self.bool()? {
            Ok(Some(self.str()?))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn path(&mut self) -> Result<PathBuf, Malformed> {
        Ok(PathBuf::from(decode_os(self.bytes()?)?))
    }

    pub(crate) fn opt_path(&mut self) -> Result<Option<PathBuf>, Malformed> {
        if self.bool()? {
            Ok(Some(self.path()?))
        } else {
            Ok(None)
        }
    }

    pub(crate) fn u32s(&mut self) -> Result<Vec<u32>, Malformed> {
        let count = self.len()?;
        // A corrupt length must not preallocate gigabytes, so grow as we read.
        let mut values = Vec::new();
        for _ in 0..count {
            values.push(self.u32()?);
        }
        Ok(values)
    }
}

/// Why a message could not be read. Deliberately coarse: the parent turns any
/// of these into "this worker did not finish", and the worker's own stderr is
/// where the real explanation lives.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Malformed {
    Unrecognized,
    Truncated,
    NotUtf8,
}

impl std::fmt::Display for Malformed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unrecognized => "unrecognized message",
            Self::Truncated => "truncated message",
            Self::NotUtf8 => "invalid UTF-8",
        })
    }
}

// A path is not text. On Unix it is arbitrary bytes, and a file named with
// something that is not UTF-8 is unusual but legal, so the raw bytes travel.
// Everywhere else `OsStr` exposes no byte view, and paths that are not valid
// Unicode are rare enough that refusing one is better than carrying a second
// encoding for it — the fan-out is a POSIX-only optimization anyway.
#[cfg(unix)]
fn encode_os(value: &std::ffi::OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(unix)]
fn decode_os(bytes: &[u8]) -> Result<OsString, Malformed> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn encode_os(value: &std::ffi::OsStr) -> Vec<u8> {
    value.to_string_lossy().into_owned().into_bytes()
}

#[cfg(not(unix))]
fn decode_os(bytes: &[u8]) -> Result<OsString, Malformed> {
    String::from_utf8(bytes.to_vec())
        .map(OsString::from)
        .map_err(|_| Malformed::NotUtf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: &[u8; 4] = b"TEST";

    #[test]
    fn round_trips_every_shape() {
        let mut writer = Writer::new(MAGIC);
        writer
            .bool(true)
            .u32(7)
            .u64(u64::MAX)
            .f32(300.5)
            .str("with\na newline\0and a null")
            .opt_str(None)
            .opt_str(Some("here"))
            .path(Path::new("/tmp/a b/c.pdf"))
            .opt_path(None)
            .u32s([3u32, 9, 27].into_iter());
        let encoded = writer.finish();

        let mut reader = Reader::new(&encoded, MAGIC).expect("magic matches");
        assert!(reader.bool().unwrap());
        assert_eq!(reader.u32().unwrap(), 7);
        assert_eq!(reader.u64().unwrap(), u64::MAX);
        assert_eq!(reader.f32().unwrap(), 300.5);
        assert_eq!(reader.str().unwrap(), "with\na newline\0and a null");
        assert_eq!(reader.opt_str().unwrap(), None);
        assert_eq!(reader.opt_str().unwrap().as_deref(), Some("here"));
        assert_eq!(reader.path().unwrap(), Path::new("/tmp/a b/c.pdf"));
        assert_eq!(reader.opt_path().unwrap(), None);
        assert_eq!(reader.u32s().unwrap(), [3, 9, 27]);
    }

    #[test]
    fn rejects_a_foreign_message() {
        assert_eq!(
            Reader::new(b"not a message", MAGIC).err(),
            Some(Malformed::Unrecognized)
        );
    }

    #[test]
    fn rejects_a_truncated_message() {
        let mut writer = Writer::new(MAGIC);
        writer.str("some text");
        let encoded = writer.finish();
        let mut reader = Reader::new(&encoded[..encoded.len() - 2], MAGIC).unwrap();
        assert_eq!(reader.str().err(), Some(Malformed::Truncated));
    }
}
