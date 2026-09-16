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
        /// The session that started this run, when another run did.
        ///
        /// Absent for a conversation a person started, and skipped rather than written as `null`, so
        /// the first line of an ordinary session is byte for byte what it always was. Recorded because
        /// a reader who opens a child's file by hand -- the tool result names the path, and reading it
        /// is how a person checks what a child did -- should not have to guess which conversation
        /// asked for it. See `SessionWriter::create` for where such a file is *put*, which is the part
        /// that keeps it out of the person's list.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    Chat {
        message: Message,
    },
    /// A peer left a message while this conversation was open.
    ///
    /// Its own event, and deliberately **not** a `Chat`: a peer's words are shown to the person and
    /// written here so the conversation can be read back whole, and they must never be part of the
    /// history that is sent to a model. Anything that can write a mailbox could otherwise steer a tool
    /// loop that has no permission layer, which is the fault `docs/agents.md` refuses to build. `load`
    /// therefore reads this event and keeps it out of `messages`; the transcript shows it at the moment
    /// it arrived, and `SessionEvent::Peer` is the record that it did.
    Peer {
        from: String,
        text: String,
        /// When it arrived, as a unix second, or 0 when the sender did not say.
        #[serde(default)]
        at: u64,
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
    /// The answer shape this conversation is being held to.
    ///
    /// `None` means the shape was *cleared* -- a run resumed with `--no-schema` -- and is written
    /// rather than left out, so "the last `schema` line wins" has an answer for going back to prose.
    ///
    /// The whole schema is written, not a path to the file it came from. A session that pointed at
    /// a file on somebody's disk would stop being readable the moment that file changed, and this
    /// format's promise is that the file is the truth: what the model was asked for, and under what
    /// contract its answer was accepted, has to be *in* the file. `--schema` also overrides it on the
    /// next run, so a changed schema is a new line rather than an edit to a written one.
    Schema {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        schema: Option<serde_json::Value>,
    },
}

/// The event names this build understands.
///
/// Used for one decision only: a line that failed to parse but names a type in here is
/// damage, and a line that names anything else is somebody else's event.
const KNOWN_TYPES: [&str; 7] = ["meta", "chat", "usage", "title", "switch", "schema", "peer"];

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
    /// The `meta` line this writer would put in the file, if it is the one that creates it.
    ///
    /// `None` when resuming, because the file is already there and already has one. Held rather than
    /// written at construction because a session file appears when the first thing is *said*, not
    /// when flint is opened -- see `append`.
    meta: Option<SessionEvent>,
}

impl SessionWriter {
    /// Start a session for this run.
    ///
    /// `parent` names the session that started *this* run, when another run did: `None` for a
    /// conversation a person started, and `main` passes what `FLINT_PARENT` said.
    ///
    /// A child's conversation is a session file like any other -- same format, same readability, and
    /// the checks in this file treat it the same -- and it is **put** somewhere else, exactly as the
    /// archive is: under `children/`, in the directory belonging to the working directory its work was
    /// held in. That is the whole mechanism: `list_detailed` reads the root and one level of project
    /// directories, so a file in there is in no listing -- `/sessions`, `flint --list-sessions`, the
    /// page's sidebar, and the numbers `--resume N` takes all agree without one filter that could
    /// drift -- and `mv` is the whole operation for a person who wants one at the top level.
    ///
    /// The reason it has to be somewhere is measured, not aesthetic: a child is *newer* than the
    /// parent that started it, so `latest_for` -- `--continue` -- answered with the child's
    /// conversation, which is a different conversation with the same prompt in it and no way to tell.
    pub fn create(
        dir: &Path,
        cwd: &Path,
        provider: &str,
        model: &str,
        parent: Option<&str>,
    ) -> Result<Self> {
        // `dir` is the sessions root; the file goes in the subdirectory that belongs to this
        // working directory. The *layout* is the separation: a caller never has to read `meta` to
        // find its own conversations, and two projects sharing one home cannot be handed each
        // other's history by anything that walks the listing. Sessions written before this live
        // directly in the root and are still found -- see `latest_for`.
        let mut dir = dir.join(dir_key(cwd));
        // A child's conversation goes one level deeper, which is the level no listing reads.
        if parent.is_some() {
            dir = dir.join(CHILDREN);
        }
        let id = new_id();
        let path = dir.join(format!("{id}.jsonl"));
        let meta = SessionEvent::Meta {
            v: FORMAT_VERSION,
            id: id.clone(),
            created: now_stamp(),
            cwd: cwd.display().to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            parent: parent.map(str::to_string),
        };
        Ok(SessionWriter {
            path,
            meta: Some(meta),
        })
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
    ///
    /// `parent` is `create`'s, and it is passed rather than defaulted because a fork made *by a child*
    /// is still a child's conversation: a copy that surfaced in the person's list would be the same
    /// surprise this exists to remove.
    pub fn seed(
        dir: &Path,
        cwd: &Path,
        provider: &str,
        model: &str,
        messages: &[Message],
        title: Option<&str>,
        parent: Option<&str>,
    ) -> Result<Self> {
        let writer = Self::create(dir, cwd, provider, model, parent)?;
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
        // A file that is not there is not a session to continue. This used to be silent, and the
        // writer then made one: with a session that had said nothing yet, its file did not exist, and
        // the first line written into it was the switch -- a file with no `meta`, which is a
        // conversation with no model and no working directory attached to it. Callers that may be
        // handed such a path check it themselves (`continue_conversation`); this is the backstop.
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "cannot resume {}: no such session file",
                path.display()
            ));
        }
        Ok(SessionWriter {
            path: path.to_path_buf(),
            // Nothing: the file is already there, and its `meta` line is already in it.
            meta: None,
        })
    }

    /// Path of the file being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event, creating the file and writing its `meta` line first if this is the first.
    ///
    /// The file is created here, by the first event, and not when the run started. Opening flint --
    /// the REPL, or `--web` -- and saying nothing is not a conversation, and a home that collects an
    /// empty file every time the page is opened is a listing of things that never happened. A run
    /// that is refused before it says anything (no key, an endpoint that cannot be reached) now
    /// leaves nothing behind at all, not even the directory.
    ///
    /// `create_new` decides whether this write is the first, rather than a flag: whoever creates the
    /// file is the one that puts `meta` in it, and nothing has to be remembered about whether that
    /// happened. A flag is wrong the moment a writer is handed on -- a switch builds a new agent for
    /// the same conversation -- and `meta` is the one line a file must not be missing.
    pub fn append(&self, event: &SessionEvent) -> Result<()> {
        // The directory goes first, and on every write rather than once: a writer can be resuming a
        // file in a directory that is not there (a home moved by hand) and the cost is one `mkdir`
        // against the open, write and flush that follow it.
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).context("cannot create sessions directory")?;
        }
        let line = serde_json::to_string(event).context("cannot serialize session event")?;
        let mut file = match OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&self.path)
        {
            Ok(file) => {
                if let Some(meta) = &self.meta {
                    let meta =
                        serde_json::to_string(meta).context("cannot serialize session event")?;
                    writeln!(&file, "{meta}").context("cannot write session event")?;
                }
                file
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
                .append(true)
                .open(&self.path)
                .with_context(|| format!("cannot open session file {}", self.path.display()))?,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("cannot open session file {}", self.path.display()))
            }
        };
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

    /// Record the answer shape this conversation is held to.
    ///
    /// `None` writes a cleared schema, for a run resumed with `--no-schema`. Append-only like
    /// everything else, so the last line is the contract in force and the history of contracts is
    /// still readable.
    pub fn schema(&self, schema: Option<&serde_json::Value>) -> Result<()> {
        self.append(&SessionEvent::Schema {
            schema: schema.cloned(),
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
    /// The answer shape in force, from the last `schema` line in the file.
    ///
    /// `None` when there has never been one, or when the last one cleared it. A resumed run holds
    /// the conversation to the same contract it was held to when it was written -- the file says
    /// what that was, and nothing outside the file has to be passed again to continue it.
    pub output_schema: Option<serde_json::Value>,
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
        output_schema: None,
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
            // Last one wins here too, and a `null` is a *cleared* schema rather than a missing field:
            // serde hands `null` over as `Some(Value::Null)`, and treating that as "no schema" is
            // what `--no-schema` means.
            Ok(SessionEvent::Schema { schema }) => {
                loaded.output_schema = schema.filter(|s| !s.is_null());
            }
            // Read, and deliberately kept out of `messages`: a peer's words belong to the person
            // reading the conversation, not to the history a model is sent. That is the whole safety
            // rule of the mailbox, and this line is where it is enforced on the way back in.
            Ok(SessionEvent::Peer { .. }) => {}
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
    /// The file this summary was read from.
    ///
    /// Carried rather than put together by a reader out of an id and the sessions root, because
    /// there is no longer one place a session can be: it lives in the subdirectory belonging to the
    /// working directory it was held in. A reader that rebuilt the path looked in the root and found
    /// nothing -- which is how `/delete 1` came to name a file that was not there.
    pub path: PathBuf,
    pub title: Option<String>,
    /// The first thing the user said, clipped.
    pub preview: String,
    pub version: u32,
    /// The directory the conversation was held in, from the `meta` line.
    ///
    /// Read so that `--continue` can mean "the conversation I was just in *here*". The session
    /// file has always recorded this -- it is what tells a reader where the work was done -- but
    /// nothing chose a session by it, so the newest file in the home won regardless of project.
    pub cwd: String,
}

impl SessionSummary {
    /// How this session is named in a listing: the name it was given, or how it started.
    ///
    /// One function because there are two listings now -- the printed one and `--list-sessions
    /// --json` -- and a rule that exists twice is a rule that drifts once: a caller reading the JSON
    /// would be comparing its own idea of the label against the one a person sees.
    pub fn label(&self) -> String {
        match self.title.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ if !self.preview.is_empty() => self.preview.clone(),
            _ => "(empty)".to_string(),
        }
    }
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
        path: path.to_path_buf(),
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
                if let Some(cwd) = value.get("cwd").and_then(|v| v.as_str()) {
                    out.cwd = cwd.to_string();
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

/// Where the conversations of runs that another run started live.
///
/// One directory per working directory, holding only child conversations -- not to be confused with
/// `archive`, which is a *decision a person made* about a conversation of their own. This one is not a
/// decision: a child's conversation is nobody's to file, and it is out of the listing by where it is
/// rather than by a flag on the file that every reader would have to honour. `SessionWriter::create`
/// has the argument; `list_detailed` needs no change at all.
const CHILDREN: &str = "children";

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

/// The name of the sessions subdirectory that belongs to a working directory.
///
/// A readable part, so that a person looking at `sessions/` can see whose conversations are whose,
/// and a hash of the whole path, so that two projects whose last component matches -- `D:\work\api`
/// and `E:\work\api` -- do not share one. The hash names a directory; it is not a security
/// boundary and nothing is authenticated with it.
///
/// Hashed from the *canonical* path, because the key has to be the answer to "which directory is
/// this" rather than "how was it spelled". Windows does not distinguish case or either separator,
/// and the same directory arrives spelled differently depending on who asked: `--cwd ..\proj`, a
/// symlink, a mapped drive, a trailing `.`. Two keys for one directory would put one project's
/// conversations in two places, which is the failure this exists to prevent -- the same one
/// `same_dir` handles for the old flat layout. When the directory cannot be canonicalised (it has
/// been deleted) the path as written is hashed instead, which is the best available answer.
pub fn dir_key(cwd: &Path) -> String {
    let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let name = canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let mut slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Long names are cut, not rejected: the hash still tells two of them apart, and a directory
    // name near the path limit would start failing for the wrong reason.
    slug.truncate(32);
    let slug = slug.trim_matches(['-', '.']).to_ascii_lowercase();
    let slug = if slug.is_empty() { "dir" } else { &slug };
    format!("{slug}-{:08x}", fnv1a(canonical.to_string_lossy().as_bytes()))
}

/// FNV-1a, 64-bit, written out rather than pulled in: one small hash over a path does not justify
/// a dependency, and this one is four lines of arithmetic that will not change.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Most recent session in `dir` that was held in `cwd`, if any.
///
/// The archive is not searched, deliberately: `--continue` means "the conversation I was
/// just in", and something filed away is not that.
///
/// The directory is part of that meaning. A conversation is tied to where it happened: the
/// files it read, the `AGENTS.md` and the skills that shaped it, and the questions that only
/// make sense there. Choosing by modification time alone made `--continue` mean "the last
/// thing anyone did anywhere in this home", so a second project's first `--continue` resumed
/// the first project's conversation and appended to it -- and a caller driving flint from a
/// program, one process per question, is exactly the case where two projects share a home and
/// never notice until the answers start referring to the other project's files.
///
/// Two places are searched, in this order: the subdirectory this working directory owns, then the
/// root, where sessions written before the split still live and are told apart by the directory
/// recorded in their `meta` line. The second is why `same_dir` is still here.
///
/// A session whose directory cannot be matched starts nothing: `--resume` names one outright,
/// and a fallback to "some other project's conversation" is the bug this exists to remove.
pub fn latest_for(dir: &Path, cwd: &Path) -> Result<Option<PathBuf>> {
    if let Some(found) = newest_in(&dir.join(dir_key(cwd)), Some(cwd))? {
        return Ok(Some(found));
    }
    newest_in(dir, Some(cwd))
}

/// The newest session file in one directory, counting only those held in `cwd` when one is given.
fn newest_in(dir: &Path, cwd: Option<&Path>) -> Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        // The `meta` line, and only the head of the file: a summary costs the same for a
        // 400 KB conversation as for a 4 KB one, which is why choosing a session this way is
        // affordable at all.
        let Ok(summary) = scan(&path) else { continue };
        if let Some(cwd) = cwd {
            if !same_dir(&summary.cwd, cwd) {
                continue;
            }
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

/// Whether a session's recorded `cwd` is the directory we are in now.
///
/// Compared as canonical paths, not as strings. Windows does not distinguish case or either
/// separator, `current_dir` may hand back a different spelling than the one that was recorded
/// -- a symlink, a `..`, a mapped drive -- and a session file is hand-editable, so the path in
/// it is not promised to be spelled the way this process spells it. When either side cannot be
/// canonicalised (a directory since removed, a file on a drive that is gone) the strings are
/// compared as written rather than the session being treated as somebody else's.
fn same_dir(recorded: &str, cwd: &Path) -> bool {
    if recorded.is_empty() {
        return false;
    }
    let recorded = Path::new(recorded);
    match (recorded.canonicalize(), cwd.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => recorded == cwd,
    }
}

/// All sessions, newest first, as (id, label) where the label is the name it was given
/// or, failing that, how it started.
pub fn list(dir: &Path) -> Result<Vec<(String, String)>> {
    Ok(list_detailed(dir)?
        .into_iter()
        .map(|s| {
            let label = s.label();
            (s.id, label)
        })
        .collect())
}

/// Like `list`, but with everything `scan` found, for the callers that print more.
///
/// Two levels are read: the root, which holds sessions written before conversations were separated
/// by directory, and one level of subdirectories, which is where they go now. The listing is about
/// all of them, inside and outside the program, so it cannot depend on which directory it was
/// asked from.
pub fn list_detailed(dir: &Path) -> Result<Vec<SessionSummary>> {
    let mut out: Vec<(std::time::SystemTime, SessionSummary)> = Vec::new();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    collect_sessions(dir, &mut out)?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        // `archive` is skipped at both levels: a session filed away is out of the listing by
        // definition, and skipping the directory here is what keeps that true without a flag on
        // any file or a filter on any reader.
        if !path.is_dir() || is_archive(&path) {
            continue;
        }
        collect_sessions(&path, &mut out)?;
    }
    // Newest first. Sorting by the timestamp and then dropping it is simpler
    // than reversing a keyed sort.
    out.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    Ok(out.into_iter().map(|(_, summary)| summary).collect())
}

/// Every archived conversation under a sessions root.
///
/// Archived files are filed *beside* the conversations they came from, so a project's archive is in
/// that project's directory and the root archive is beside the root's own files. Searching all of
/// them is what keeps `--resume <id>` honest: an archived conversation is still one you may want to
/// read, and filing it away -- or moving a file by hand -- must not make it unreachable.
pub fn list_archived(root: &Path) -> Result<Vec<SessionSummary>> {
    let mut out = list_detailed(&archive_dir(root))?;
    for entry in std::fs::read_dir(root)?.flatten() {
        let path = entry.path();
        if !path.is_dir() || is_archive(&path) {
            continue;
        }
        out.extend(list_detailed(&archive_dir(&path))?);
    }
    Ok(out)
}

/// Whether a path is the archive directory.
fn is_archive(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()) == Some("archive")
}

/// Add every session file directly in `dir` to `out`.
///
/// Directly in it: the walk one level out is the caller's, because the two levels mean different
/// things -- the root holds the older layout and the project directories, and only one of those is
/// walked into.
fn collect_sessions(
    dir: &Path,
    out: &mut Vec<(std::time::SystemTime, SessionSummary)>,
) -> Result<()> {
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
    Ok(())
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

    /// The same line, held in a directory of the caller's choosing.
    ///
    /// The path is escaped, because this is JSON and a Windows path is nothing but backslashes:
    /// written raw, `"cwd":"C:\Users\..."` is not JSON at all, the line is skipped as damage, and
    /// a test of "which directory was this held in" would pass by skipping everything.
    fn meta_in(id: &str, cwd: &str) -> String {
        let cwd = cwd.replace('\\', "\\\\");
        format!(
            r#"{{"type":"meta","v":1,"id":"{id}","created":"epoch:1","cwd":"{cwd}","provider":"p","model":"m"}}"#
        )
    }

    fn user(text: &str) -> String {
        format!(r#"{{"type":"chat","message":{{"role":"user","content":"{text}"}}}}"#)
    }

    /// `--continue` means "the conversation I was just in *here*", not "the newest file anywhere".
    ///
    /// The old choice was by modification time over the whole home, so a second project's first
    /// `--continue` resumed the first project's conversation and appended to it, with nothing said.
    /// One home with one directory per project is exactly the shape a program driving flint -- one
    /// process per question -- ends up in, and it is precisely where the answers would start
    /// referring to the wrong project's files.
    #[test]
    fn a_session_is_continued_only_in_the_directory_it_was_held_in() {
        let home = TempDir::new("latest-for-home");
        let project = TempDir::new("latest-for-project");
        let other = TempDir::new("latest-for-other");
        let project_cwd = project.0.display().to_string();
        let other_cwd = other.0.display().to_string();

        let mine = home.file(
            "111-1.jsonl",
            &[meta_in("111-1", &project_cwd), user("in the project")],
        );
        // Newer, and in another directory: the mistake this test exists for is picking this one.
        let theirs = home.file(
            "222-1.jsonl",
            &[meta_in("222-1", &other_cwd), user("somewhere else")],
        );

        assert_eq!(
            latest_for(&home.0, &project.0).expect("latest_for"),
            Some(mine.clone()),
            "the newest session in the home was picked over the one held in this directory"
        );
        assert_eq!(
            latest_for(&home.0, &other.0).expect("latest_for"),
            Some(theirs),
            "the other directory's own conversation must still be found from there"
        );

        // The same directory spelled differently -- a trailing `.` is what a caller with a
        // relative path hands over -- is still the same directory.
        assert_eq!(
            latest_for(&home.0, &project.0.join(".")).expect("latest_for"),
            Some(mine),
            "a differently spelled path to the same directory did not match"
        );

        // Nothing has happened here yet, and the honest answer is nothing: not another
        // directory's conversation.
        let untouched = TempDir::new("latest-for-untouched");
        assert_eq!(
            latest_for(&home.0, &untouched.0).expect("latest_for"),
            None,
            "a directory with no conversation of its own was given somebody else's"
        );

        // A `meta` line with no `cwd` -- hand-written, or older than the field -- belongs to
        // nobody, so it cannot be resumed by accident from anywhere.
        home.file("333-1.jsonl", &[meta("333-1", Some(1)), user("no cwd")]);
        assert_eq!(
            latest_for(&home.0, &untouched.0).expect("latest_for"),
            None,
            "a session that does not say where it was held was claimed by a directory"
        );
    }

    /// A working directory gets a name of its own: readable, and stable.
    ///
    /// Readable because `sessions/` is a directory people look at, and stable because this name is
    /// what keeps two projects sharing one home apart. A key that changed with the spelling of the
    /// path would put one project's conversations in two places -- the failure the separation exists
    /// to prevent. The hash is what keeps two projects whose *last component* matches apart:
    /// `D:\work\api` and `E:\work\api` are different projects, and a name made of the last component
    /// alone would not say so.
    #[test]
    fn a_working_directory_gets_a_name_of_its_own() {
        let root = TempDir::new("dir-key");
        let one = root.0.join("api");
        let two = root.0.join("work").join("api");
        std::fs::create_dir_all(&one).expect("one");
        std::fs::create_dir_all(&two).expect("two");

        let key = dir_key(&one);
        assert!(key.starts_with("api-"), "not readable: {key}");
        assert!(
            key.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_'),
            "a key that is not a single path component: {key}"
        );
        // The same directory, spelled the way whichever process asked happened to spell it.
        assert_eq!(key, dir_key(&one.join(".")), "one directory, two keys");
        assert_eq!(
            key,
            dir_key(&one.join("..").join("api")),
            "one directory, two keys"
        );
        // Two projects whose last component matches are still two projects.
        assert_ne!(key, dir_key(&two), "two projects, one key: {key}");
        // A directory with nothing usable in its name still gets one.
        assert!(dir_key(&root.0).contains('-'), "no hash in the key");
    }

    /// A child's conversation is a session file like any other, and it is not one of yours.
    ///
    /// Reported from a real session: a `task` child's conversation appeared in `/sessions` and in the
    /// page's sidebar exactly like a conversation the person had. Nothing was wrong with the file --
    /// it is the same format, and reading it is how a person checks what a child did -- but "the
    /// newest conversation in this directory" stopped meaning "mine" the moment a run could start
    /// runs, and `--continue` really did resume a child's session two minutes after its parent was
    /// interrupted.
    ///
    /// The location is the whole mechanism, exactly as it is for the archive: the listing reads the
    /// root and one level of project directories, so a file under `children/` is not in it without any
    /// filter that could drift -- and `mv` is the whole operation for a person who wants one at the
    /// top level.
    #[test]
    fn a_childs_session_is_kept_out_of_the_listing_by_where_it_lives() {
        let root = TempDir::new("session-children");
        let project = TempDir::new("session-children-project");

        let mine = SessionWriter::create(&root.0, &project.0, "p", "m", None).expect("create");
        mine.title("mine").expect("title");
        let child = SessionWriter::create(&root.0, &project.0, "p", "m", Some("1789456770-557"))
            .expect("create");
        child.title("the child's").expect("title");

        assert_eq!(
            child.path().parent().and_then(|d| d.file_name()),
            Some(std::ffi::OsStr::new("children")),
            "the child's session is not in its own directory: {}",
            child.path().display()
        );
        let meta = std::fs::read_to_string(child.path()).expect("read");
        let first = meta.lines().next().expect("a meta line");
        assert!(
            first.contains(r#""parent":"1789456770-557""#),
            "the child's own record does not say which conversation asked for it: {first}"
        );

        // The listing: one conversation, the person's, whatever else is on disk.
        let listed = list_detailed(&root.0).expect("list");
        assert_eq!(
            listed.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec![mine.path().file_stem().unwrap().to_str().unwrap()],
            "the child's conversation is in the person's list"
        );
        // `--continue` is the same question asked by name, and it used to answer with the child.
        assert_eq!(
            latest_for(&root.0, &project.0).expect("latest"),
            Some(mine.path().to_path_buf()),
            "--continue found something that is not the person's conversation"
        );
    }

    /// A new session is written into the subdirectory of the run it belongs to.
    ///
    /// The layout *is* the separation, so it is worth asserting where the file lands rather than
    /// only that it can be found: a file in the root is a file no project owns, and it is exactly
    /// what `--continue` would then have to guess about.
    #[test]
    fn a_new_session_lives_with_the_directory_it_was_held_in() {
        let root = TempDir::new("session-layout");
        let project = TempDir::new("session-layout-project");
        let other = TempDir::new("session-layout-other");
        let writer = SessionWriter::create(&root.0, &project.0, "p", "m", None).expect("create");
        let path = writer.path().to_path_buf();
        writer.title("a name").expect("title");
        assert_eq!(
            path.parent(),
            Some(root.0.join(dir_key(&project.0)).as_path()),
            "the session did not land in its own directory: {}",
            path.display()
        );
        // And it is found again from there, which is the point of putting it there.
        assert_eq!(
            latest_for(&root.0, &project.0).expect("latest"),
            Some(path.clone())
        );
        // A directory with nothing of its own gets nothing, rather than somebody else's.
        assert_eq!(latest_for(&root.0, &other.0).expect("latest"), None);
    }

    /// A new session writes nothing until something is said.
    ///
    /// Reported from the page, which is where it is most obvious: opening `--web` created a session
    /// file before anyone had typed a word, so the sidebar filled up with conversations that never
    /// happened and `/sessions` numbered them. The file -- and the directory it goes in -- is created
    /// by the first event now, and `meta` is still the first line, because a file whose first line is
    /// not `meta` is a conversation with no model and no working directory attached to it.
    #[test]
    fn a_session_file_appears_when_something_is_said() {
        let root = TempDir::new("lazy-create");
        let project = TempDir::new("lazy-create-project");
        let writer = SessionWriter::create(&root.0, &project.0, "p", "m", None).expect("create");
        assert!(
            !writer.path().exists(),
            "a session file was written before anything was said: {}",
            writer.path().display()
        );
        assert!(
            !root.0.join(dir_key(&project.0)).exists(),
            "the sessions directory was created for a run that said nothing"
        );

        writer
            .append(&SessionEvent::Chat {
                message: Message::User {
                    content: "the first thing said".to_string(),
                },
            })
            .expect("append");
        let text = std::fs::read_to_string(writer.path()).expect("read the session");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "not one line per event: {text:?}");
        assert!(
            lines[0].contains(r#""type":"meta""#),
            "`meta` is not the first line: {text:?}"
        );
        assert!(
            lines[1].contains("the first thing said"),
            "the first thing said is not in the file: {text:?}"
        );

        // Resuming does not write a second `meta`: the file already has one.
        let resumed = SessionWriter::resume(writer.path()).expect("resume");
        resumed.title("a name").expect("title");
        let text = std::fs::read_to_string(writer.path()).expect("read the session");
        assert_eq!(
            text.matches(r#""type":"meta""#).count(),
            1,
            "resuming wrote a second `meta`: {text:?}"
        );
        assert!(text.contains(r#""type":"title""#), "{text:?}");
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
        let writer = SessionWriter::create(&dir.0, Path::new("/tmp"), "p", "m", None).unwrap();
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
        let writer = SessionWriter::create(&dir.0, Path::new("/tmp"), "p", "m", None).unwrap();
        // Named, because a session file exists from the first thing said -- see
        // `a_session_file_appears_when_something_is_said`.
        writer.title("a name").unwrap();
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
            None,
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
