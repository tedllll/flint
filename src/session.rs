//! Session persistence: append-only JSONL.
//!
//! Chosen over SQLite on purpose. A rescue tool's own state must be readable
//! and repairable with a text editor, and must not require a C toolchain to
//! build. One JSON object per line, nothing more.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::event::{Message, Usage};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SessionEvent {
    /// First line of every session; identifies it and pins the environment.
    Meta {
        id: String,
        created: String,
        cwd: String,
        provider: String,
        model: String,
    },
    Chat {
        message: Message,
    },
    Usage {
        usage: Usage,
    },
}

/// Append handle for a session file.
pub struct SessionWriter {
    path: PathBuf,
}

impl SessionWriter {
    pub fn create(dir: &Path, cwd: &Path, provider: &str, model: &str) -> Result<Self> {
        std::fs::create_dir_all(dir).context("cannot create sessions directory")?;
        let id = new_id();
        let path = dir.join(format!("{id}.jsonl"));
        let meta = SessionEvent::Meta {
            id: id.clone(),
            created: now_iso8601(),
            cwd: cwd.display().to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
        };
        let writer = SessionWriter { path };
        writer.append(&meta)?;
        Ok(writer)
    }

    /// Reopen an existing session file so the conversation keeps being saved.
    ///
    /// Resuming used to be read-only, which meant the answers you gave after
    /// `--continue` were written nowhere. A conversation you cannot save is
    /// barely a conversation, and the whole point of resuming is to carry on.
    pub fn resume(path: &Path) -> Result<Self> {
        Ok(SessionWriter {
            path: path.to_path_buf(),
        })
    }

    /// Path of the file being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event. Flushed immediately so a crash loses at most one event.
    pub fn append(&self, event: &SessionEvent) -> Result<()> {
        let line = serde_json::to_string(event).context("cannot serialize session event")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("cannot open session file {}", self.path.display()))?;
        writeln!(file, "{line}").context("cannot write session event")?;
        file.flush().context("cannot flush session event")?;
        Ok(())
    }
}

/// Everything needed to resume a session.
pub struct LoadedSession {
    pub id: String,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub messages: Vec<Message>,
    pub last_usage: Option<Usage>,
}

/// Read a session file, tolerating (and reporting) damaged lines.
pub fn load(path: &Path) -> Result<LoadedSession> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read session {}", path.display()))?;

    let mut loaded = LoadedSession {
        id: path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        cwd: String::new(),
        provider: String::new(),
        model: String::new(),
        messages: Vec::new(),
        last_usage: None,
    };

    let mut damaged = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionEvent>(line) {
            Ok(SessionEvent::Meta {
                id,
                cwd,
                provider,
                model,
                ..
            }) => {
                loaded.id = id;
                loaded.cwd = cwd;
                loaded.provider = provider;
                loaded.model = model;
            }
            Ok(SessionEvent::Chat { message }) => loaded.messages.push(message),
            Ok(SessionEvent::Usage { usage }) => loaded.last_usage = Some(usage),
            Err(_) => damaged += 1,
        }
    }

    if damaged > 0 {
        eprintln!(
            "flint: {damaged} unreadable line(s) skipped in {}",
            path.display()
        );
    }
    Ok(loaded)
}

/// Most recent session in `dir`, if any.
pub fn latest(dir: &Path) -> Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if newest.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            newest = Some((modified, path));
        }
    }
    Ok(newest.map(|(_, p)| p))
}

/// All sessions, newest first, as (id, summary of first user message).
pub fn list(dir: &Path) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(std::time::SystemTime, String, String)> = Vec::new();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(loaded) = load(&path) else { continue };
        let summary = loaded
            .messages
            .iter()
            .find_map(|m| match m {
                Message::User { content } => Some(crate::util::preview(content, 60)),
                _ => None,
            })
            .unwrap_or_else(|| "(empty)".to_string());
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        out.push((modified, loaded.id, summary));
    }
    // Newest first. Sorting by the timestamp and then dropping it is simpler
    // than reversing a keyed sort.
    out.sort_by_key(|(modified, _, _)| std::cmp::Reverse(*modified));
    Ok(out
        .into_iter()
        .map(|(_, id, summary)| (id, summary))
        .collect())
}

fn new_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{}", now.as_secs(), now.subsec_millis())
}

/// UTC timestamp without pulling in a date crate.
fn now_iso8601() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch:{secs}")
}
