//! Byte-offset watermarks, partial-line buffers and rotation detection (plan §7).
//!
//! The collector must never re-read consumed bytes: a codex rollout file can
//! reach tens of megabytes and a full backfill of the corpus measured ~42s
//! excluding JSON parsing.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// Per-file read state.
#[derive(Debug, Default)]
pub struct FileTail {
    /// Byte offset just past the last byte consumed. Advanced even when the tail
    /// of that range was an incomplete line, because that line's bytes are kept
    /// in `residue` and must not be re-read.
    pub offset: u64,
    /// Trailing bytes that did not end in a newline, re-prefixed on the next read.
    pub residue: Vec<u8>,
    /// Absolute offset of `residue[0]`, so a completed line can be reported with
    /// its true starting offset (which becomes part of the dedup key).
    pub residue_start: u64,
}

/// A line carved out of a fresh byte range.
#[derive(Debug, Clone)]
pub struct TailLine {
    /// Byte offset of the line's first byte inside the file.
    pub start: u64,
    /// The line without its terminating newline.
    pub bytes: Vec<u8>,
}

/// What a read pass did, so the caller can decide whether to advance the watermark.
#[derive(Debug, Default)]
pub struct TailOutcome {
    pub lines: Vec<TailLine>,
    /// New offset to persist. A pass reads at most one chunk.
    pub offset: u64,
    /// Set when the file shrank: the caller must clear per-file derived state
    /// (codex `turn_context`, grok per-`sid` model cache) because the file was
    /// rotated or truncated and re-read from zero.
    pub rotated: bool,
}

/// Read the bytes appended since `tail.offset` and split them into lines.
///
/// A trailing partial line stays in `tail.residue` and is not reported: it may
/// be split mid-write, and parsing a truncated JSON object would either fail
/// loudly or (worse) succeed with missing fields.
pub fn read_appended(path: &Path, tail: &mut FileTail, size: u64) -> std::io::Result<TailOutcome> {
    const READ_CHUNK: u64 = 256 * 1024;
    let mut outcome = TailOutcome::default();
    if size < tail.offset {
        // Rotation or truncation: discard the watermark and the residue.
        tail.offset = 0;
        tail.residue.clear();
        tail.residue_start = 0;
        outcome.rotated = true;
    }
    if size == tail.offset {
        outcome.offset = tail.offset;
        return Ok(outcome);
    }
    let end = size.min(tail.offset.saturating_add(READ_CHUNK));
    let bytes = read_range(path, tail.offset, end)?;
    let prefix = std::mem::take(&mut tail.residue);
    let start = tail.residue_start;
    tail.residue_start = start;
    split_lines(
        start,
        &prefix,
        &bytes,
        &mut tail.residue,
        &mut tail.residue_start,
        &mut outcome.lines,
    );
    tail.offset = end;
    outcome.offset = end;
    Ok(outcome)
}

/// Offset bookkeeping shared by omp / codex / grok.
///
/// A file that has never been seen before is *baselined*: the store records its
/// current size and reads nothing. That is the "不回填历史" rule (plan §7.2) —
/// without it, pointing the collector at an existing corpus would import every
/// byte of history on the first round.
#[derive(Debug, Default)]
pub struct OffsetStore {
    tails: BTreeMap<String, FileTail>,
    baselined: std::collections::HashSet<String>,
    /// `true` when the store was built from a persisted cursor: the collector
    /// has run before, so an unseen-but-live file is a *new* log (collect it
    /// from zero) rather than part of the historical corpus (baseline it).
    resumed: bool,
}

impl OffsetStore {
    pub fn from_cursor(cursor: Option<&str>) -> Self {
        let tails = cursor
            .and_then(|text| serde_json::from_str::<crate::event::FileOffsets>(text).ok())
            .map(|decoded| {
                decoded
                    .offsets
                    .into_iter()
                    .map(|(path, offset)| {
                        let residue = decoded.residues.get(&path);
                        (
                            path,
                            FileTail {
                                offset,
                                residue: residue
                                    .map(|value| value.text.as_bytes().to_vec())
                                    .unwrap_or_default(),
                                residue_start: residue.map(|value| value.start).unwrap_or(offset),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            tails,
            baselined: Default::default(),
            resumed: cursor.is_some(),
        }
    }

    /// Whether this store was built from a persisted cursor (a prior round
    /// already ran) as opposed to a cold start.
    pub fn is_resumed(&self) -> bool {
        self.resumed
    }

    pub fn tail(&mut self, path: &Path) -> &mut FileTail {
        self.tails
            .entry(path.to_string_lossy().into_owned())
            .or_default()
    }

    /// Whether `path` already carries a watermark (loaded or baselined this run).
    pub fn is_known(&self, path: &Path) -> bool {
        self.tails
            .contains_key(&path.to_string_lossy().into_owned())
    }

    /// The persisted watermarks, for tests and diagnostics.
    pub fn offset_for(&self, path: &Path) -> Option<u64> {
        self.tails
            .get(&path.to_string_lossy().into_owned())
            .map(|tail| tail.offset)
    }

    /// Adopt `path` at its current `size` without reading it, for a file that has
    /// no watermark yet. Returns the offset to persist.
    pub fn baseline(&mut self, path: &Path, size: u64) -> u64 {
        let key = path.to_string_lossy().into_owned();
        self.baselined.insert(key.clone());
        let tail = self.tails.entry(key).or_default();
        tail.residue.clear();
        tail.residue_start = size;
        tail.offset = size;
        size
    }

    pub fn forget(&mut self, path: &Path) {
        self.tails.remove(&path.to_string_lossy().into_owned());
    }

    pub fn cursor(&self) -> String {
        let offsets = self
            .tails
            .iter()
            .map(|(path, tail)| (path.clone(), tail.offset))
            .collect();
        let residues = self
            .tails
            .iter()
            .filter(|(_, tail)| !tail.residue.is_empty())
            .filter_map(|(path, tail)| {
                String::from_utf8(tail.residue.clone()).ok().map(|text| {
                    (
                        path.clone(),
                        crate::event::Residue {
                            start: tail.residue_start,
                            text,
                        },
                    )
                })
            })
            .collect();
        serde_json::to_string(&crate::event::FileOffsets { offsets, residues })
            .unwrap_or_else(|_| "{\"offsets\":{},\"residues\":{}}".into())
    }
}

/// Enumerate files under `root` whose name matches `predicate`.
pub fn collect_files(root: &Path, predicate: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files_recursive(root, &predicate, &mut files);
    files
}

fn collect_files_recursive(
    dir: &Path,
    predicate: &impl Fn(&Path) -> bool,
    files: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_file() {
            if predicate(&path) {
                files.push(path);
            }
        } else if file_type.is_dir() {
            collect_files_recursive(&path, predicate, files);
        }
    }
}

/// Split `bytes` into complete lines, carrying any trailing fragment into `residue`.
fn split_lines(
    start: u64,
    prefix: &[u8],
    bytes: &[u8],
    residue: &mut Vec<u8>,
    residue_start: &mut u64,
    lines: &mut Vec<TailLine>,
) {
    let mut buffer: Vec<u8> = Vec::with_capacity(prefix.len() + bytes.len());
    buffer.extend_from_slice(prefix);
    buffer.extend_from_slice(bytes);
    let mut line_start = start;
    let mut cursor = 0usize;
    while let Some(position) = buffer[cursor..].iter().position(|byte| *byte == b'\n') {
        let end = cursor + position;
        lines.push(TailLine {
            start: line_start,
            bytes: buffer[cursor..end].to_vec(),
        });
        cursor = end + 1;
        line_start = start + cursor as u64;
    }
    residue.clear();
    residue.extend_from_slice(&buffer[cursor..]);
    *residue_start = line_start;
}

/// Read a byte range without loading the whole file.
pub fn read_range(path: &Path, from: u64, to: u64) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut buffer = vec![0u8; (to - from) as usize];
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_backlog_is_read_in_chunks_and_keeps_cross_chunk_line_offsets() {
        let path = std::env::temp_dir().join(format!("herdr-tail-{}.jsonl", std::process::id()));
        let first = vec![b'x'; 256 * 1024 + 17];
        let mut contents = first.clone();
        contents.extend_from_slice(b"\nsecond\npartial");
        std::fs::write(&path, &contents).unwrap();
        let mut tail = FileTail::default();
        let mut lines = Vec::new();
        let mut chunks = 0;
        while tail.offset < contents.len() as u64 {
            let outcome = read_appended(&path, &mut tail, contents.len() as u64).unwrap();
            assert!(outcome.offset <= contents.len() as u64);
            chunks += 1;
            lines.extend(outcome.lines);
        }
        assert!(chunks > 1);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start, 0);
        assert_eq!(lines[0].bytes, first);
        assert_eq!(lines[1].start, first.len() as u64 + 1);
        assert_eq!(lines[1].bytes, b"second");
        assert_eq!(tail.residue, b"partial");
        let _ = std::fs::remove_file(path);
    }
}
