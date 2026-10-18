//! Append-only JSONL history. Retention is applied on read (and by occasional compaction).

use crate::pipeline::Outcome;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Unix time, milliseconds.
    pub ts_ms: u64,
    pub raw: String,
    pub cleaned: Option<String>,
    pub final_text: String,
    pub asr_ms: u64,
    pub cleanup_ms: u64,
    pub cleanup_used: bool,
    pub error: Option<String>,
}

impl Entry {
    pub fn from_outcome(o: &Outcome) -> Self {
        Self {
            ts_ms: now_ms(),
            raw: o.raw.clone(),
            cleaned: o.cleaned.clone(),
            final_text: o.final_text.clone(),
            asr_ms: o.timings.asr_ms,
            cleanup_ms: o.timings.cleanup_ms,
            cleanup_used: o.cleanup_used,
            error: o.error.clone(),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `dirs::data_dir()/capcom/history.jsonl`.
pub fn default_path() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("capcom").join("history.jsonl")
}

pub struct History {
    path: PathBuf,
    keep: usize,
}

impl History {
    pub fn new(path: PathBuf, keep: usize) -> Self {
        Self { path, keep: keep.max(1) }
    }

    pub fn open_default(keep: usize) -> Self {
        Self::new(default_path(), keep)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry (file created 0600). Compacts when the file grows past 2x `keep`.
    pub fn append(&self, e: &Entry) -> Result<()> {
        if let Some(p) = self.path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&self.path)?;
        writeln!(f, "{}", serde_json::to_string(e)?)?;
        drop(f);
        self.maybe_compact()?;
        Ok(())
    }

    /// Last `n` entries (oldest first), bounded by `keep`. Corrupt lines are skipped.
    pub fn read_last(&self, n: usize) -> Result<Vec<Entry>> {
        let s = match std::fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        Ok(tail_entries(&s, n.min(self.keep)))
    }

    pub fn last(&self) -> Result<Option<Entry>> {
        Ok(self.read_last(1)?.pop())
    }

    fn maybe_compact(&self) -> Result<()> {
        let s = std::fs::read_to_string(&self.path)?;
        if s.lines().count() <= self.keep * 2 {
            return Ok(());
        }
        let keep: Vec<&str> = {
            let lines: Vec<&str> = s.lines().collect();
            lines[lines.len() - self.keep..].to_vec()
        };
        let tmp = self.path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, keep.join("\n") + "\n")?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Parse the last `n` valid entries from JSONL text, oldest first.
pub fn tail_entries(s: &str, n: usize) -> Vec<Entry> {
    let mut v: Vec<Entry> = s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    if v.len() > n {
        v.drain(..v.len() - n);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(t: &str) -> Entry {
        Entry {
            ts_ms: 1,
            raw: t.into(),
            cleaned: None,
            final_text: t.into(),
            asr_ms: 1,
            cleanup_ms: 0,
            cleanup_used: false,
            error: None,
        }
    }

    #[test]
    fn append_read_last() {
        let d = tempfile::tempdir().unwrap();
        let h = History::new(d.path().join("sub/h.jsonl"), 3);
        assert!(h.last().unwrap().is_none());
        for i in 0..5 {
            h.append(&e(&format!("t{i}"))).unwrap();
        }
        let v = h.read_last(10).unwrap();
        assert_eq!(v.iter().map(|x| x.raw.as_str()).collect::<Vec<_>>(), ["t2", "t3", "t4"]);
        assert_eq!(h.last().unwrap().unwrap().raw, "t4");
        assert_eq!(h.read_last(1).unwrap()[0].raw, "t4");
    }

    #[test]
    fn compaction_bounds_file() {
        let d = tempfile::tempdir().unwrap();
        let h = History::new(d.path().join("h.jsonl"), 2);
        for i in 0..20 {
            h.append(&e(&format!("t{i}"))).unwrap();
        }
        let lines = std::fs::read_to_string(h.path()).unwrap().lines().count();
        assert!(lines <= 5, "{lines}");
        assert_eq!(h.last().unwrap().unwrap().raw, "t19");
    }

    #[test]
    fn skips_corrupt_lines() {
        let line = serde_json::to_string(&e("ok")).unwrap();
        let v = tail_entries(&format!("garbage\n{line}\n{{\"x\":1}}\n"), 5);
        assert_eq!(v.len(), 1);
    }
}
