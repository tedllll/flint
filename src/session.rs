//! Session persistence: append-only JSONL.
//!
//! Chosen over SQLite on purpose. A rescue tool's own state must be readable
//! and repairable with a text editor, and must not require a C toolchain to
//! build. One JSON object per line, nothing more.
//!
//! Three rules follow from that, and the code is easier to read with them stated
//! first:
//!
//! - **Nothing is ever rewritten.** Naming a conversation appends a `title` line
//!   instead of editing `Meta`; archiving moves the file instead of marking a
//!   record. A file that is only ever appended to cannot be half-rewritten by a
//!   crash, and what happened stays in it in the order it happened.
//! - **An unknown line is not damage.** A newer flint, or a person with an
//!   editor, may leave an event type this build has never heard of. Skipping it
//!   quietly is what makes the format extensible: reporting it as damage would
//!   turn every future version into "corruption" for this one.
//! - **Listing must not read everything.** A conversation that grew to hundreds
//!   of kilobytes still only needs two lines' worth of information to appear in
//!   a list, so a list reads the two ends of each file and nothing else.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::event::{Message, Usage};

/// The format revision this build writes into `Meta`.
///
/// v1 is every file written before a conversation could be named. The field has a
/// default, so a file that never mentions a version is v1 and a hand-written or
/// hand-trimmed file still loads -- which is the whole reason it is a defaulted field
/// rather than a required one.
pub const FORMAT_VERSION: u32 = 2;

fn version_one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SessionEvent {
    /// First line of every session; identifies it and pins the environment.
    Meta {
        #[serde(default = "version_one")]
        v: u32,
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
    /// A human name for the conversation.
    ///
    /// Its own event rather than a field on `Meta` because the file is append-only: the
    /// last `title` in the file is the one in force, so renaming costs one line and needs
    /// no rewrite of anything already written.
    Title { name: String },
    /// The provider or model in force changed.
    ///
    /// `/model`, `/provider` and `/reload` each replace the agent around a conversation that stays
    /// where it is, so this is written to the file that conversation is in. `Meta` records where the
    /// session *started* -- "which model said that, and from where?" -- and the last `switch` is
    /// where it is now, which is what `load` reports and `--resume` acts on. Before this event
    /// existed a switch seeded a whole second file, and one conversation became two.
    Switch { provider: String, model: String },
}

/// The event names this build understands.
///
/// Used for one decision only: a line that failed to parse but names a type in here is
/// damage, and a line that names anything else is somebody else's event.
const KNOWN_TYPES: [&str; 5] = ["meta", "chat", "usage", "title", "switch"];

/// Whether a line names an event type this build knows.
fn names_a_known_event(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(str::to_string))
        .map(|t| KNOWN_TYPES.contains(&t.as_str()))
        .unwrap_or(false)
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
            v: FORMAT_VERSION,
            id: id.clone(),
            created: now_stamp(),
            cwd: cwd.display().to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
        };
        let writer = SessionWriter { path };
        writer.append(&meta)?;
        Ok(writer)
    }

    /// Start a new file that already holds a conversation.
    ///
    /// For a conversation that is being **copied** rather than continued: `--fork` asks for a second
    /// conversation that begins where the first one is, and a copy has to be a file of its own.
    /// Switching model or provider used to come through here as well, and no longer does -- it
    /// appends a `Switch` to the file the conversation is already in, which is why one conversation
    /// stays one conversation when the run moves to another model. The old reason for seeding (a new
    /// agent "wants a file of its own", because `Meta` names the model and `resume` believes it) is
    /// gone with it: `load` reports the last `Switch`, and that is the model in force.
    ///
    /// `title` travels with it, because a name is a line in the file it was given to and a
    /// switch that quietly renamed a conversation would be the same kind of loss this exists
    /// to prevent.
    pub fn seed(
        dir: &Path,
        cwd: &Path,
        provider: &str,
        model: &str,
        messages: &[Message],
        title: Option<&str>,
    ) -> Result<Self> {
        let writer = Self::create(dir, cwd, provider, model)?;
        for message in messages {
            // The system prompt is not part of the conversation: it is rebuilt for every run
            // from the machine flint is on, so a copy written here would come back through
            // `/resume` as a message -- a stale one, from another directory or another build.
            if matches!(message, Message::System { .. }) {
                continue;
            }
            writer.append(&SessionEvent::Chat {
                message: message.clone(),
            })?;
        }
        if let Some(name) = title {
            writer.title(name)?;
        }
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

    /// Name the conversation. Appending is what keeps the file append-only.
    pub fn title(&self, name: &str) -> Result<()> {
        self.append(&SessionEvent::Title {
            name: name.to_string(),
        })
    }

    /// Record that the run moved to another provider or model.
    ///
    /// An event rather than a new file, and the argument is the one `Usage` already makes for its own
    /// numbers: the file is append-only, so what changed is a line in it, and the last line is what
    /// is in force. Switching used to seed a second file holding the whole conversation, which made
    /// one conversation into two files -- two rows in the page's sidebar, two numbers in `/sessions`,
    /// and half a conversation behind either of them.
    pub fn switched(&self, provider: &str, model: &str) -> Result<()> {
        self.append(&SessionEvent::Switch {
            provider: provider.to_string(),
            model: model.to_string(),
        })
    }
}

/// Everything needed to resume a session.
pub struct LoadedSession {
    pub id: String,
    /// The format revision the file declares. v1 when it declares nothing.
    pub version: u32,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    /// The last `title` in the file, if it was ever named.
    pub title: Option<String>,
    pub messages: Vec<Message>,
    pub last_usage: Option<Usage>,
}

/// Read a session file, tolerating (and reporting) damaged lines.
///
/// "Damaged" and "unknown" are deliberately different verdicts. Damage is a line this
/// build should have been able to read and could not, and it is reported, because a
/// transcript with a hole in it is something the reader has to know about. An unknown
/// event is a line meant for a different version, and it is skipped without comment.
pub fn load(path: &Path) -> Result<LoadedSession> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read session {}", path.display()))?;

    let mut loaded = LoadedSession {
        id: path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        // A file with no `Meta` at all is not claiming to be new, so it gets the oldest
        // revision: better to report a line we cannot read than to hide it.
        version: version_one(),
        cwd: String::new(),
        provider: String::new(),
        model: String::new(),
        title: None,
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
                v,
                id,
                cwd,
                provider,
                model,
                ..
            }) => {
                loaded.version = v;
                loaded.id = id;
                loaded.cwd = cwd;
                loaded.provider = provider;
                loaded.model = model;
            }
            Ok(SessionEvent::Chat { message }) => loaded.messages.push(message),
            Ok(SessionEvent::Usage { usage }) => loaded.last_usage = Some(usage),
            Ok(SessionEvent::Title { name }) => loaded.title = Some(name),
            // Read in order, so the last one wins: a conversation that moved twice is held with the
            // provider and model it moved to last, which is what `--resume` acts on.
            Ok(SessionEvent::Switch { provider, model }) => {
                loaded.provider = provider;
                loaded.model = model;
            }
            // Skipped in silence when the file claims a newer revision -- a type this
            // build knows may have changed shape in it, and that is not damage either.
            Err(_) if loaded.version > FORMAT_VERSION || !names_a_known_event(line) => {}
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

/// What a session looks like without reading all of it.
#[derive(Debug, Clone, Default)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    /// The first thing the user said, clipped.
    pub preview: String,
    pub version: u32,
}

/// Bytes read from each end of a file to summarise it.
///
/// The head carries `Meta` and the first thing the user said; the tail carries any
/// `title`, because naming appends and the newest name is therefore the last line. Both
/// are generous for what they hold -- the point is that listing a 400 KB conversation
/// costs the same as listing a 4 KB one.
const HEAD_BYTES: u64 = 64 * 1024;
const TAIL_BYTES: u64 = 16 * 1024;

/// Summarise a session by reading its two ends.
pub fn scan(path: &Path) -> Result<SessionSummary> {
    let len = std::fs::metadata(path)
        .with_context(|| format!("cannot stat {}", path.display()))?
        .len();

    let mut out = SessionSummary {
        id: path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        version: version_one(),
        ..Default::default()
    };

    // Head. Lossy on purpose: a cut in the middle of a multi-byte character is expected
    // when the file is longer than the window, and it must not turn into an error.
    let mut head = Vec::new();
    std::fs::File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?
        .take(HEAD_BYTES)
        .read_to_end(&mut head)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let head = String::from_utf8_lossy(&head);
    let mut lines: Vec<&str> = head.lines().collect();
    if len > HEAD_BYTES {
        // Only if there is more file: otherwise the last line is whole.
        lines.pop();
    }

    for line in lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("meta") => {
                out.version = value
                    .get("v")
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as u32)
                    .unwrap_or_else(version_one);
                if let Some(id) = value.get("id").and_then(|v| v.as_str()) {
                    out.id = id.to_string();
                }
            }
            Some("chat") => {
                // The first user message, not the latest: a list is for recognising a
                // conversation you were in, and how it started is what does that.
                let message = value.get("message");
                let role = message
                    .and_then(|m| m.get("role"))
                    .and_then(|r| r.as_str());
                if out.preview.is_empty() && role == Some("user") {
                    let content = message
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or_default();
                    out.preview = crate::util::preview(content, 60);
                }
            }
            _ => {}
        }
    }

    // Tail. Read even when the head covered the whole file: the two are cheap, and
    // branching on "was it short enough" is how one of the ends ends up unread.
    let mut tail = Vec::new();
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    if len > TAIL_BYTES {
        file.seek(SeekFrom::End(-(TAIL_BYTES as i64)))
            .with_context(|| format!("cannot seek in {}", path.display()))?;
    }
    file.take(TAIL_BYTES)
        .read_to_end(&mut tail)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let tail = String::from_utf8_lossy(&tail);
    // The first line is cut in half when the tail is a slice rather than the whole file.
    for line in tail.lines().skip(usize::from(len > TAIL_BYTES)) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) == Some("title") {
            if let Some(name) = value.get("name").and_then(|n| n.as_str()) {
                out.title = Some(name.to_string());
            }
        }
    }

    Ok(out)
}

/// Where archived conversations live.
///
/// A subdirectory rather than a flag on `Meta`, because `mv` is then the whole
/// operation: something a person can do, undo, or script by hand, and something that
/// keeps archived sessions out of the listing without any code that filters them.
pub fn archive_dir(dir: &Path) -> PathBuf {
    dir.join("archive")
}

/// Move a session into the archive, returning where it went.
pub fn archive(path: &Path) -> Result<PathBuf> {
    let here = path.parent().unwrap_or_else(|| Path::new("."));
    let target_dir = if here.file_name().and_then(|n| n.to_str()) == Some("archive") {
        here.to_path_buf()
    } else {
        archive_dir(here)
    };
    std::fs::create_dir_all(&target_dir).context("cannot create the archive directory")?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("not a session file: {}", path.display()))?;
    let target = target_dir.join(name);
    std::fs::rename(path, &target)
        .with_context(|| format!("cannot archive {}", path.display()))?;
    Ok(target)
}

/// Delete a session file.
///
/// No trash directory: an archived session already is the reversible half of this, and a
/// hidden copy of something the user asked to delete is a worse surprise than the
/// deletion itself.
pub fn delete(path: &Path) -> Result<()> {
    std::fs::remove_file(path).with_context(|| format!("cannot delete {}", path.display()))
}

/// Most recent session in `dir`, if any.
///
/// The archive is not searched, deliberately: `--continue` means "the conversation I was
/// just in", and something filed away is not that.
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

/// All sessions, newest first, as (id, label) where the label is the name it was given
/// or, failing that, how it started.
pub fn list(dir: &Path) -> Result<Vec<(String, String)>> {
    Ok(list_detailed(dir)?
        .into_iter()
        .map(|s| {
            let label = match s.title.as_deref().map(str::trim) {
                Some(name) if !name.is_empty() => name.to_string(),
                _ if !s.preview.is_empty() => s.preview.clone(),
                _ => "(empty)".to_string(),
            };
            (s.id, label)
        })
        .collect())
}

/// Like `list`, but with everything `scan` found, for the callers that print more.
pub fn list_detailed(dir: &Path) -> Result<Vec<SessionSummary>> {
    let mut out: Vec<(std::time::SystemTime, SessionSummary)> = Vec::new();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(summary) = scan(&path) else { continue };
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        out.push((modified, summary));
    }
    // Newest first. Sorting by the timestamp and then dropping it is simpler
    // than reversing a keyed sort.
    out.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    Ok(out.into_iter().map(|(_, summary)| summary).collect())
}

fn new_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{}", now.as_secs(), now.subsec_millis())
}

/// When the session started, as `epoch:<seconds>`.
///
/// Not a date format, whatever this used to be called: Unix seconds with a prefix that says
/// so. That is enough to order sessions and to show a person roughly when one was written,
/// and it needs no date library to produce or to parse -- which, for one field, is the whole
/// argument.
fn now_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("epoch:{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A directory of this test's own, removed when the guard drops.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "flint-session-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("temp dir");
            TempDir(path)
        }

        fn file(&self, name: &str, lines: &[String]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write session");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn meta(id: &str, version: Option<u32>) -> String {
        match version {
            Some(v) => format!(
                r#"{{"type":"meta","v":{v},"id":"{id}","created":"epoch:1","cwd":"/tmp","provider":"p","model":"m"}}"#
            ),
            None => format!(
                r#"{{"type":"meta","id":"{id}","created":"epoch:1","cwd":"/tmp","provider":"p","model":"m"}}"#
            ),
        }
    }

    fn user(text: &str) -> String {
        format!(r#"{{"type":"chat","message":{{"role":"user","content":"{text}"}}}}"#)
    }

    /// A switch is a line in the file, not a second file.
    ///
    /// Switching provider or model used to seed a **new** session with the whole conversation in it,
    /// because `Meta` names the provider and model and `load` believes it. That made one conversation
    /// into two files with the same messages in them: the sidebar grew a row nobody asked for, the
    /// list numbered the same conversation twice, and a `--resume` of either half was half a
    /// conversation. `Usage` had already made this argument for its own numbers -- a file that is
    /// append-only records what changed *when* it changed -- so a switch is an event, the last one
    /// wins, and `load` reports the model actually in force.
    #[test]
    fn a_switch_is_a_line_and_the_last_one_is_believed() {
        use crate::event::Message;

        let dir = TempDir::new("switch");
        let writer = SessionWriter::create(&dir.0, Path::new("/tmp"), "p", "m").unwrap();
        writer
            .append(&SessionEvent::Chat {
                message: Message::User {
                    content: "before".to_string(),
                },
            })
            .unwrap();
        writer.switched("q", "m2").unwrap();
        writer.switched("q", "m3").unwrap();
        writer
            .append(&SessionEvent::Chat {
                message: Message::User {
                    content: "after".to_string(),
                },
            })
            .unwrap();

        let text = std::fs::read_to_string(writer.path()).unwrap();
        assert!(
            text.contains(r#"{"type":"switch","provider":"q","model":"m3"}"#),
            "the switch is not a line of its own in the file: {text}"
        );
        let loaded = load(writer.path()).unwrap();
        assert_eq!(loaded.model, "m3", "the last switch is not the model in force");
        assert_eq!(loaded.provider, "q", "the provider in force is the meta's, not the switch's");
        assert_eq!(loaded.messages.len(), 2, "the conversation is still one conversation");
        assert_eq!(
            std::fs::read_dir(&dir.0).unwrap().count(),
            1,
            "a switch wrote a second file"
        );
    }

    #[test]
    fn the_writer_records_the_current_format_version() {
        let dir = TempDir::new("version");
        let writer = SessionWriter::create(&dir.0, Path::new("/tmp"), "p", "m").unwrap();
        let text = std::fs::read_to_string(writer.path()).unwrap();
        assert!(
            text.contains(&format!(r#""v":{FORMAT_VERSION}"#)),
            "the file does not declare its format version: {text}"
        );
        dir.file("old.jsonl", &[meta("old", None), user("hi")]);
        let loaded = load(&dir.0.join("old.jsonl")).unwrap();
        assert_eq!(loaded.version, 1, "a file without a version is v1");
    }

    /// A seeded file is an ordinary session that happens to start with a conversation.
    ///
    /// The two things worth holding still: the system prompt is *not* written (it is rebuilt
    /// for every run, and a copy in the file would come back through `/resume` as a stale
    /// message), and what is written is everything else, in order, under the new model -- a
    /// conversation that arrives at `/resume` one message short is a conversation that
    /// disagrees with the terminal that produced it.
    #[test]
    fn a_seeded_file_holds_the_conversation_and_not_the_prompt() {
        use crate::event::Message;

        let dir = TempDir::new("seed");
        let messages = vec![
            Message::system("the prompt as it was on the day"),
            Message::user("what was said"),
            Message::Assistant {
                content: Some("what was answered".to_string()),
                reasoning: None,
                tool_calls: Vec::new(),
            },
        ];
        let writer = SessionWriter::seed(
            &dir.0,
            Path::new("/tmp"),
            "other",
            "other-model",
            &messages,
            Some("a name"),
        )
        .unwrap();

        let loaded = load(writer.path()).unwrap();
        assert_eq!(loaded.provider, "other");
        assert_eq!(loaded.model, "other-model");
        assert_eq!(loaded.title.as_deref(), Some("a name"));
        assert!(
            !loaded
                .messages
                .iter()
                .any(|m| matches!(m, Message::System { .. })),
            "the system prompt was written into the file: {:?}",
            loaded.messages
        );
        assert_eq!(loaded.messages.len(), 2, "{:?}", loaded.messages);
        assert!(
            std::fs::read_to_string(writer.path())
                .unwrap()
                .contains("what was said")
        );
    }

    #[test]
    fn a_title_names_the_session_and_the_last_one_wins() {
        let dir = TempDir::new("title");
        let path = dir.file(
            "t.jsonl",
            &[
                meta("t", Some(2)),
                user("fix the codex config"),
                r#"{"type":"title","name":"codex config"}"#.to_string(),
                user("and this too"),
                r#"{"type":"title","name":"codex, again"}"#.to_string(),
            ],
        );

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.title.as_deref(), Some("codex, again"));
        assert_eq!(loaded.messages.len(), 2);

        let summary = scan(&path).unwrap();
        assert_eq!(summary.title.as_deref(), Some("codex, again"));
        assert_eq!(summary.preview, "fix the codex config");
        assert_eq!(list(&dir.0).unwrap()[0].1, "codex, again");
    }

    /// The property that makes the format extensible: a file from a future version must
    /// not be reported as damaged, and its lines must not be silently counted as chat.
    #[test]
    fn an_unknown_event_type_is_skipped_in_silence() {
        let dir = TempDir::new("unknown");
        let path = dir.file(
            "u.jsonl",
            &[
                meta("u", Some(2)),
                user("hello"),
                r#"{"type":"compaction","summary":"earlier stuff"}"#.to_string(),
            ],
        );
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.version, 2);
    }

    /// A file that says it is newer may have changed the shape of an event this build
    /// knows; that is not damage either.
    #[test]
    fn a_newer_format_is_not_reported_as_damage() {
        let dir = TempDir::new("newer");
        let path = dir.file(
            "n.jsonl",
            &[
                meta("n", Some(FORMAT_VERSION + 1)),
                r#"{"type":"chat","message":{"role":"user","content":["blocks now"]}}"#
                    .to_string(),
            ],
        );
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.version, FORMAT_VERSION + 1);
        assert!(loaded.messages.is_empty());
    }

    /// Damage is still damage: a line naming an event this build knows, which it cannot
    /// read, means the transcript has a hole in it.
    #[test]
    fn a_known_type_that_cannot_be_read_counts_as_damage() {
        let dir = TempDir::new("damaged");
        let path = dir.file(
            "d.jsonl",
            &[
                meta("d", Some(2)),
                r#"{"type":"chat"}"#.to_string(),
                "not json at all".to_string(),
            ],
        );
        let loaded = load(&path).unwrap();
        assert!(loaded.messages.is_empty());
    }

    /// The point of `scan`: a name appended after a very long conversation is still
    /// found, and found from the end of the file rather than by reading all of it.
    #[test]
    fn a_name_at_the_end_of_a_long_file_is_found() {
        let dir = TempDir::new("long");
        let mut lines = vec![meta("long", Some(2)), user("start")];
        // Comfortably more than either window, so the name and the first message cannot
        // both be inside the head.
        for i in 0..4000 {
            lines.push(format!(
                r#"{{"type":"chat","message":{{"role":"assistant","content":"line {i} {}"}}}}"#,
                "x".repeat(60)
            ));
        }
        lines.push(r#"{"type":"title","name":"buried"}"#.to_string());
        let path = dir.file("long.jsonl", &lines);
        assert!(std::fs::metadata(&path).unwrap().len() > HEAD_BYTES);

        let summary = scan(&path).unwrap();
        assert_eq!(summary.title.as_deref(), Some("buried"));
        assert_eq!(summary.preview, "start");
    }

    #[test]
    fn archiving_moves_the_file_out_of_the_listing_and_keeps_it_loadable() {
        let dir = TempDir::new("archive");
        let path = dir.file("a.jsonl", &[meta("a", Some(2)), user("keep me")]);

        let moved = archive(&path).unwrap();
        assert!(!path.exists(), "the file is still in the sessions directory");
        assert!(moved.starts_with(archive_dir(&dir.0)));
        assert_eq!(list(&dir.0).unwrap().len(), 0);
        assert_eq!(load(&moved).unwrap().messages.len(), 1);
        // Archiving again is a no-op rather than an error: the path is already there.
        assert_eq!(archive(&moved).unwrap(), moved);
    }

    #[test]
    fn deleting_removes_the_file() {
        let dir = TempDir::new("delete");
        let path = dir.file("d.jsonl", &[meta("d", Some(2)), user("bye")]);
        delete(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(list(&dir.0).unwrap().len(), 0);
    }
}
