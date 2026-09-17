//! Who else is working in this directory.
//!
//! Two agents in one checkout is the accident this exists for: they collide over a file, or one
//! commits a revision the other is still writing, and neither can see it coming. `ROADMAP.md`'s
//! "being used by another agent" section records the incident that made it concrete. `docs/agents.md`
//! is the plan this is stages 1 and 3 of.
//!
//! It answers with two signals that must never be blended into one sentence, because they have
//! different authors and different precision:
//!
//! - a **presence record**, written by a running flint and refreshed while it lives. Exact — and
//!   blind to everything that is not flint.
//! - **recent changes** on disk (`git status`, file mtimes). True of every tool on the machine, and
//!   unable to name an author.
//!
//! Beside them lives the **mailbox**: one append-only JSONL file per directory, where a peer -- a
//! person at a terminal, another flint, a script -- leaves a message for whoever is working there.
//! The safety rule that makes it a mailbox rather than an injection channel is in `docs/agents.md`
//! and is enforced here by what this module does *not* do: it hands the words to the transcript and
//! to the session file, and nothing in it can put a peer's words into a request to a model.
//!
//! "No other agent is here" is the sentence that must never be wrong, so both are reported and both
//! are labelled: the first names a run, the second names files and says plainly that it does not know
//! who touched them. Presence sees flint and only flint; two installations with different
//! `FLINT_HOME`s cannot see each other at all, which is written down rather than papered over.
//!
//! Nothing here is derived state in the sense the repository forbids: a record is *live* state, like
//! a pid file, and cannot be recovered from the session file — an mtime says a file was written, not
//! that a process is still working. A record whose writer died is not an error either: it is reported
//! as stale, because a killed process cannot clean up and a listing that hid those would be lying
//! about the one case that matters (`kill -9`, a suspended process, a machine that lost power).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How often a live record is refreshed, and how long one may go unrefreshed before it is reported as
/// stale rather than alive.
///
/// The refresh is unconditional -- it does not depend on the model being called or a tool running --
/// because the failure that matters is calling a live run dead. A long `bash` command, a slow
/// provider, a person reading a diff: none of them may look like a dead process. Twelve missed
/// refreshes is far longer than any pause that is not a stall, and a suspended process is reported as
/// stale on purpose: it is not going to notice anything.
pub const TOUCH_EVERY: Duration = Duration::from_secs(5);
pub const STALE_AFTER: Duration = Duration::from_secs(60);

/// How far back the "changed recently" line looks, and how many files it names before it stops.
pub const RECENT_WINDOW: Duration = Duration::from_secs(600);
const RECENT_LIMIT: usize = 12;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A run's claim that it is alive, as it is written to disk.
///
/// Hand-editable and hand-repairable like everything else: one JSON object, one file, under
/// `<FLINT_HOME>/live/`. The file name carries the nonce so that two records in one process never
/// share a name -- a rebuilt agent, a `--web` viewer and a run beside it -- which is what makes
/// removing one on exit unable to remove another's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Presence {
    pub pid: u32,
    pub cwd: PathBuf,
    pub provider: String,
    pub model: String,
    pub readonly: bool,
    /// When the run began, and when it last said so. Both as Unix seconds, so a person reading the
    /// file with `notepad` has something they can subtract.
    pub started: u64,
    pub last_seen: u64,
    /// The conversation this run is writing, as a path, empty until there is one.
    ///
    /// A path rather than the id, because the id is not enough to find the file: which directory keys
    /// it, and whether it sits in `children/`, are not reconstructible from the name -- the same reason
    /// `--list-sessions --json` carries paths. Defaulted on read so that a record written by an older
    /// build, or by hand, is still a record rather than damage.
    #[serde(default)]
    pub session: String,
    /// The nonce is in the *file name* and not in the record: a reader never needs it, and only the
    /// run that wrote a record ever removes one. Two guards in one process -- a rebuilt agent, a
    /// `--web` viewer beside a run -- therefore cannot delete each other's file.
    #[serde(skip)]
    nonce: String,
}

impl Presence {
    pub fn is_stale(&self) -> bool {
        now_secs().saturating_sub(self.last_seen) > STALE_AFTER.as_secs()
    }

    /// How long ago the run last said it was alive, in words a person reads at a glance.
    pub fn seen_ago(&self) -> String {
        let secs = now_secs().saturating_sub(self.last_seen);
        if secs < 60 {
            format!("{secs}s ago")
        } else if secs < 3600 {
            format!("{}m ago", secs / 60)
        } else {
            format!("{}h ago", secs / 3600)
        }
    }

    pub fn age(&self) -> String {
        let secs = now_secs().saturating_sub(self.started);
        if secs < 60 {
            format!("{secs}s")
        } else if secs < 3600 {
            format!("{}m", secs / 60)
        } else {
            format!("{}h", secs / 3600)
        }
    }

    /// The file name is the run's identity on disk: pid plus nonce, the same in every directory this
    /// record is written to. That is what lets a reader that sees two copies of one record count it
    /// once, and what makes removing one on exit unable to remove another run's.
    fn file_in(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}-{}.json", self.pid, self.nonce))
    }

    fn file(&self) -> PathBuf {
        self.file_in(&crate::config::live_dir())
    }
}

/// A record that is being kept alive, and removed when it is dropped.
///
/// Dropped rather than closed by hand, so that every exit path is covered by construction: an early
/// return, an error, a panic that unwinds. `kill -9` leaves the file behind, which is the case
/// [`Presence::is_stale`] exists for.
pub struct Guard {
    /// Shared with the refresh thread, because the session is not known when the record starts: the
    /// writer is created a moment later, and a run that is announcing itself before it has a
    /// conversation is the normal case rather than an edge one.
    record: std::sync::Arc<std::sync::Mutex<Presence>>,
    /// Held separately: the file name is pid plus nonce and never changes, so `Drop` can remove the
    /// record without taking the lock.
    path: PathBuf,
    /// The project's copy of the same record, when the project keeps flint state in `.flint/`.
    ///
    /// `None` is the common case and means what it says: this checkout never asked for flint's
    /// project state, so nothing of flint's is written into it. See [`crate::config::project_dir`].
    mirror: Option<PathBuf>,
    /// Closed when the guard is dropped, which is what wakes the thread immediately.
    ///
    /// A channel rather than an `AtomicBool` beside a `sleep`: the thread is *joined* on the way out,
    /// so a plain sleep makes every run take up to [`TOUCH_EVERY`] longer to exit than it should.
    /// That is not a theory -- an existing test with a timing bound measured the 5.019 s and failed,
    /// which is the whole reason the wait here has a timeout instead of being a sleep.
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Guard {
    /// Start announcing this run, and keep announcing it until the guard is dropped.
    pub fn begin(cwd: &Path, provider: &str, model: &str, readonly: bool) -> Guard {
        let started = now_secs();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_else(|_| started.to_string());
        let record = Presence {
            pid: std::process::id(),
            cwd: cwd.to_path_buf(),
            provider: provider.to_string(),
            model: model.to_string(),
            readonly,
            started,
            last_seen: started,
            session: String::new(),
            nonce,
        };
        if let Err(e) = write_record(&record) {
            // A run that cannot announce itself still runs. Saying so on stderr is the honest
            // version: the alternative is either a silent failure to appear in `flint who`, or
            // refusing to work because a directory could not be created.
            eprintln!("flint: warning: cannot write {}: {e}", record.file().display());
        }

        // The project's copy, when the project has opted in by keeping a `.flint/` directory. A
        // failure here is reported for the same reason and is not fatal for the same reason: this
        // copy is how a run with a different `FLINT_HOME` is seen, and a run that silently stopped
        // being seen across installations is the failure the whole marker exists to prevent.
        let mirror = crate::config::project_dir(cwd).map(|dir| dir.join("live"));
        if let Some(dir) = &mirror {
            if let Err(e) = write_record_in(dir, &record) {
                eprintln!(
                    "flint: warning: cannot write {}: {e}",
                    record.file_in(dir).display()
                );
            }
        }

        let path = record.file();
        // Poisoning is ignored on purpose: the record is a single JSON object and the worst a panic
        // under the lock can leave behind is a stale one, which readers already handle by design.
        let shared = std::sync::Arc::new(std::sync::Mutex::new(record));
        let (stop, wake) = std::sync::mpsc::channel::<()>();
        let thread = {
            let shared = std::sync::Arc::clone(&shared);
            let mirror = mirror.clone();
            std::thread::Builder::new()
                .name("flint-live".to_string())
                .spawn(move || {
                    // A timeout is the tick, and a closed channel is the signal to stop. The refresh
                    // does not depend on the model being called or a tool running: a long `bash`
                    // command, a slow provider or a person reading a diff must not look like a dead
                    // process, because "no other agent here" is the answer that must never be wrong.
                    while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                        wake.recv_timeout(TOUCH_EVERY)
                    {
                        let mut record = shared.lock().unwrap_or_else(|e| e.into_inner());
                        record.last_seen = now_secs();
                        let _ = write_record(&record);
                        if let Some(dir) = &mirror {
                            let _ = write_record_in(dir, &record);
                        }
                    }
                })
                .ok()
        };
        Guard {
            record: shared,
            path,
            mirror,
            stop: Some(stop),
            thread,
        }
    }

    /// Say which conversation this run is holding.
    ///
    /// Written at once rather than at the next refresh: a parent that starts a child asks "which
    /// conversation is that" immediately, and a five-second-old answer is the difference between a
    /// handle and a guess. Called with the same path more than once is the normal case -- every turn
    /// of a REPL -- so an unchanged session costs nothing but the comparison.
    pub fn set_session(&self, session: &Path) {
        let text = session.display().to_string();
        let mut record = self.record.lock().unwrap_or_else(|e| e.into_inner());
        if record.session == text {
            return;
        }
        record.session = text;
        let _ = write_record(&record);
        if let Some(dir) = &self.mirror {
            let _ = write_record_in(dir, &record);
        }
    }

    /// Where this run's record is, for a message that wants to name it.
    ///
    /// The home's copy, which is the one that exists whether or not the project keeps flint state.
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Dropping the sender closes the channel, which ends the thread's wait at once.
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
        if let Some(dir) = &self.mirror {
            let record = self.record.lock().unwrap_or_else(|e| e.into_inner());
            let _ = std::fs::remove_file(record.file_in(dir));
        }
    }
}

/// Write a record where a reader can never see half of one.
///
/// Through a temporary file and a rename, because `flint who` may read this at any moment and a
/// truncated JSON object would be reported as damage -- a warning the reader cannot act on, produced
/// by flint itself, is worse than the microseconds a rename costs.
fn write_record(record: &Presence) -> std::io::Result<()> {
    write_record_in(&crate::config::live_dir(), record)
}

/// The same, into one named directory: the home's `live/`, or a project's `.flint/live/`.
fn write_record_in(dir: &Path, record: &Presence) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = record.file_in(dir);
    let text = serde_json::to_string(record)?;
    let temp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
    }
    std::fs::rename(&temp, &path)
}

/// Every record on this machine, split by whether it is still claiming to be alive.
pub struct Listing {
    pub alive: Vec<Presence>,
    pub stale: Vec<Presence>,
    /// Files that are not records. Reported rather than skipped: a record someone hand-edited into
    /// nonsense is exactly the thing a person wants to be told about, and silence would look like
    /// "no other agent".
    pub unreadable: Vec<(PathBuf, String)>,
}

/// Every record this machine's home knows about.
pub fn scan() -> Listing {
    scan_in(&std::env::current_dir().unwrap_or_default())
}

/// Every record this *directory* can see: the home's, and a project's when the project keeps one.
///
/// Two directories rather than one because the two records are one record: a run inside a project
/// that has a `.flint/` writes the same file -- same pid, same nonce -- in both places, so the file
/// *name* is the identity and a reader that sees both copies counts one run. That is why the home is
/// read first: a record in both is one run, and the home's copy is the one that cannot be missing.
///
/// This is the whole of what the project marker buys, and it is worth stating plainly: with two
/// installations whose `FLINT_HOME`s differ, this is the only way either sees the other. Nothing
/// else about the record changes -- the staleness window, the unreadable-line report and the "names
/// no author" rule for the changed-files line are all the same facts in a second directory.
pub fn scan_in(cwd: &Path) -> Listing {
    let mut listing = Listing {
        alive: Vec::new(),
        stale: Vec::new(),
        unreadable: Vec::new(),
    };
    // One entry per file name, and the later word about a run wins: a pair of records where one was
    // refreshed and the other was not is a half-written pair, not two runs. Equal timestamps leave
    // whichever was read first, which is the home's -- the copy that cannot be missing.
    let mut freshest: std::collections::HashMap<String, Presence> =
        std::collections::HashMap::new();
    read_records(&crate::config::live_dir(), &mut listing, &mut freshest);
    if let Some(project) = crate::config::project_dir(cwd) {
        read_records(&project.join("live"), &mut listing, &mut freshest);
    }
    for record in freshest.into_values() {
        if record.is_stale() {
            listing.stale.push(record);
        } else {
            listing.alive.push(record);
        }
    }
    listing
        .alive
        .sort_by_key(|record| std::cmp::Reverse(record.last_seen));
    listing
        .stale
        .sort_by_key(|record| std::cmp::Reverse(record.last_seen));
    listing
}

/// Read one directory's records into `freshest`, and its damage into `listing`.
///
/// Keyed by file name rather than by pid because the name is what a run writes into both
/// directories, and because two runs may share a pid across a reboot -- the nonce is what separates
/// them, and the nonce is in the name.
fn read_records(
    dir: &Path,
    listing: &mut Listing,
    freshest: &mut std::collections::HashMap<String, Presence>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // No directory is the normal state of a machine where nothing has run yet, and of a project
        // that has a `.flint/` but has never had a run in it.
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        match std::fs::read_to_string(&path).map(|t| serde_json::from_str::<Presence>(&t)) {
            Ok(Ok(record)) => match freshest.get(&name) {
                Some(older) if older.last_seen >= record.last_seen => {}
                _ => {
                    freshest.insert(name, record);
                }
            },
            Ok(Err(e)) => listing.unreadable.push((path, e.to_string())),
            Err(e) => listing.unreadable.push((path, e.to_string())),
        }
    }
}

/// A file that changed recently, with the only two facts the filesystem will admit to.
pub struct Changed {
    pub path: String,
    pub secs_ago: u64,
}

/// What a directory looks like right now, without attributing any of it to anybody.
pub struct Recent {
    /// Lines from `git status --porcelain`, when this is a repository. Empty otherwise.
    pub git_lines: Vec<String>,
    pub changed: Vec<Changed>,
    /// Set when the directory is not a repository, or git could not be asked: the difference between
    /// "nothing changed" and "nothing could be looked at" is the whole honesty of this signal.
    pub note: Option<String>,
}

/// Look for recent changes in `cwd`, and never claim to know who made them.
///
/// Two sources, in this order. A git repository is asked first because it knows what is *different*
/// rather than merely recent -- an untouched checkout reports nothing, while a full mtime scan of a
/// build directory reports noise. The files it names are then checked against the clock, because git
/// status says nothing about when. A directory that is not a repository gets no mtime walk of its
/// own: a bounded walk would either be slow or would quietly miss the file that mattered, and a
/// signal that is wrong about "nothing here" is the one thing this must not produce. It says so
/// instead.
pub fn recent(cwd: &Path, window: Duration) -> Recent {
    let mut recent = Recent {
        git_lines: Vec::new(),
        changed: Vec::new(),
        note: None,
    };
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["status", "--porcelain"])
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            recent.git_lines = text.lines().map(|l| l.to_string()).collect();
        }
        Ok(_) => recent.note = Some("git status failed in this directory".to_string()),
        Err(_) => {
            recent.note = Some("git is not available, so changes cannot be attributed to a clock".to_string())
        }
    }

    let cutoff = window.as_secs();
    for line in &recent.git_lines {
        // `XY path`, and a rename as `XY old -> new`: the path is what matters and the last field is
        // the one that exists now.
        let path = line
            .get(3..)
            .map(|rest| rest.rsplit(" -> ").next().unwrap_or(rest).trim().to_string())
            .unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        let full = cwd.join(&path);
        if let Some(secs_ago) = modified_secs_ago(&full) {
            if secs_ago <= cutoff {
                recent.changed.push(Changed { path, secs_ago });
            }
        }
    }
    recent.changed.sort_by_key(|c| c.secs_ago);
    recent.changed.truncate(RECENT_LIMIT);
    recent
}

/// Seconds since the file was last written, or `None` when there is nothing to stat.
pub fn modified_secs_ago(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    Some(age.as_secs())
}

/// The newest session file belonging to `cwd`, and how long ago it was written.
///
/// The third signal, and the weakest: it works after the fact (a run that has ended still leaves its
/// conversation behind), it only sees flint, and it cannot tell a run that is thinking from one that
/// was killed. It is here because "which conversation is being written to right now" is a question
/// `flint who` should be able to answer about a process it cannot otherwise see.
pub fn newest_session(cwd: &Path) -> Option<(PathBuf, u64)> {
    let dir = crate::config::sessions_dir().join(crate::session::dir_key(cwd));
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut newest: Option<(PathBuf, u64)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(age) = modified_secs_ago(&path) {
            if newest.as_ref().map(|(_, best)| age < *best).unwrap_or(true) {
                newest = Some((path, age));
            }
        }
    }
    newest
}

/// One message a peer left for whoever is working in this directory.
///
/// `from` is what the sender *says* it is, and it is worth no more than that: anything that can write
/// the file can write that field. It is shown as a claim ("`x` says:") rather than as an identity, and
/// nothing branches on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerMessage {
    pub from: String,
    pub text: String,
    /// A run's pid or session id when it was addressed to one, empty for "whoever is here".
    pub to: String,
    pub at: u64,
}

/// The mailbox for one directory: `<project>/.flint/mailbox.jsonl` when the directory is inside a
/// project that keeps flint state there, and otherwise the home's, one file per directory key.
///
/// **The project's file wins rather than being written beside the home's**, and that is the one
/// place the mailbox differs from the presence record. A record is *state* -- the same facts in two
/// directories, deduplicated by name -- while a mailbox is an append-only log of *events*, and two
/// logs holding the same message would show it twice to a run that can see both, with no way to tell
/// a duplicate from somebody saying the same sentence again. So there is one mailbox per directory:
/// the project's when the project asked for it, which is also what makes two installations able to
/// hear each other at all.
pub fn mailbox_path(cwd: &Path) -> PathBuf {
    if let Some(project) = crate::config::project_dir(cwd) {
        return project.join("mailbox.jsonl");
    }
    crate::config::mailbox_dir().join(format!("{}.jsonl", crate::session::dir_key(cwd)))
}

/// Leave a message for whoever is working in `cwd`.
///
/// Append-only, one JSON object per line, so a person can read it with `type` and a peer can `tail`
/// it. Written with one `write` call: a mailbox line is small enough that an append is atomic in
/// practice, and a torn line is reported as unreadable rather than skipped when it is read back.
pub fn say(cwd: &Path, from: &str, to: &str, text: &str) -> anyhow::Result<PathBuf> {
    Ok(say_line(cwd, from, to, text)?.0)
}

/// The same, handing back the exact line that was appended.
///
/// A run that says something from its own prompt writes into the mailbox it is following, and has to
/// be able to tell its own line from a peer's when it reads the file back. It cannot recognise it by
/// the `from` field -- that field is a claim, and anything that can write the file can write it -- so
/// the writer is handed the bytes and the reader is handed them back. Splitting this out rather than
/// rebuilding the line at the call site keeps the two from drifting when a field is added.
pub fn say_line(cwd: &Path, from: &str, to: &str, text: &str) -> anyhow::Result<(PathBuf, String)> {
    use anyhow::Context;
    let path = mailbox_path(cwd);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let record = serde_json::json!({
        "from": from,
        "to": to,
        "at": now_secs(),
        "cwd": cwd.to_string_lossy(),
        "text": text,
    });
    let mut line = serde_json::to_string(&record)?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    use std::io::Write;
    file.write_all(line.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok((path, line))
}

/// Who will see a message left in this directory, as one sentence.
///
/// A pure function because the honesty is in the wording, and the wrong wording is the easy one: a
/// mailbox is a *file*, so "nobody is here" is not a failure and must not read like one, and a list of
/// pids must not be dressed up as a promise that they are watching. It is also the sentence that says
/// whether anybody is listening at all, which is what a person leaving a note actually wants to know.
pub fn audience(others: &[Presence]) -> String {
    match others {
        [] => "nobody else is working here right now — it waits in the file for the next run".to_string(),
        [one] => format!(
            "pid {} is working here; it shows what arrives between turns",
            one.pid
        ),
        many => {
            let pids: Vec<String> = many.iter().map(|p| p.pid.to_string()).collect();
            format!(
                "{} runs are working here (pid {}); each shows it between turns",
                many.len(),
                pids.join(", ")
            )
        }
    }
}

/// What a person is told after leaving a message, for both doors (`flint say` and `/say`).
///
/// Shared rather than written twice, and the sentence about the model is why: it used to read "it is
/// never sent to a model", which stopped being true when `--hear-peers` was built -- a listener that
/// asked to hear peers passes what it hears to its own model. The sender cannot know which listeners
/// asked, so the honest sentence names the possibility instead of denying it.
pub fn say_reply(text: &str, path: &Path, to: &str, here: &str) -> String {
    let mut out = format!("said: {text}\n  in: {}\n", path.display());
    if !to.is_empty() {
        out.push_str(&format!("  to: {to}\n"));
    }
    out.push_str(&format!("  here: {here}\n"));
    out.push_str(
        "  (a run working here shows it to its person; a run started with --hear-peers also passes \
         it to its model)",
    );
    out
}

/// A reader that follows one directory's mailbox, showing what arrives *after* it started.
///
/// Starting at the end of the file rather than at the beginning is the whole design: a run shows
/// messages that arrive while it is working, not the archive of everything ever said here. A fresh
/// run therefore begins quiet, which is the honest default -- and it means the cursor is not state
/// that has to be persisted, only a byte offset this process remembers.
#[derive(Debug)]
pub struct Mailbox {
    path: PathBuf,
    /// Byte offset of the first line not yet shown, and the identity of the file it refers to. A file
    /// that shrank (a hand-edit, a new `FLINT_HOME` directory) is re-read from the start rather than
    /// skipped: reading from beyond the end would silently lose every message after it.
    cursor: u64,
    len: u64,
    /// Lines this run wrote itself, waiting to be skipped when the reader reaches them.
    ///
    /// Without this a run that leaves a message from its own prompt reads it back as a peer's on the
    /// next turn boundary, and neither the person nor a run asked to hear peers can tell whose words
    /// they are. Matching the *exact bytes* the writer appended is deliberate: recognising them by the
    /// `from` field would be trusting a claim (anything that can write the file can write `pid 12345`),
    /// and the worst a forged duplicate buys is hiding the forger's own message.
    own: Vec<String>,
}

impl Mailbox {
    /// Follow the mailbox of `cwd`, from wherever it is now.
    pub fn following(cwd: &Path) -> Self {
        let path = mailbox_path(cwd);
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Mailbox {
            path,
            cursor: len,
            len,
            own: Vec::new(),
        }
    }

    /// Remember a line this run wrote, so that reading the mailbox back does not hand it to the person
    /// as somebody else's words. Takes the bytes `live::say_line` returned rather than rebuilding them.
    pub fn note_own(&mut self, line: String) {
        self.own.push(line);
    }

    /// Messages addressed to `me` (a pid or a session id) or to nobody in particular.
    ///
    /// A line that cannot be parsed is reported as a peer message that says so rather than dropped:
    /// two agents failing to talk because one wrote a bad line is exactly the silence this is for
    /// breaking, and the person watching should see it happen.
    pub fn new_messages(&mut self, me: &str) -> Vec<PeerMessage> {
        let Ok(text) = std::fs::read_to_string(&self.path) else {
            return Vec::new();
        };
        let len = text.len() as u64;
        if len < self.cursor {
            self.cursor = 0;
        }
        self.len = len;
        if len == self.cursor {
            return Vec::new();
        }
        let fresh = &text[self.cursor as usize..];
        // Only complete lines are consumed: a peer may be mid-write, and the rest of a line read now
        // would be a message nobody sent. The cursor stops at the last newline.
        let consumed = fresh.rfind('\n').map(|i| i + 1).unwrap_or(0);
        if consumed == 0 {
            return Vec::new();
        }
        self.cursor += consumed as u64;
        peer_messages(&fresh[..consumed], me, &mut self.own)
    }
}

/// The complete lines of one mailbox read, as the messages this run should be shown.
///
/// Split out of `Mailbox::new_messages` so the filter that matters -- a run must not be shown its own
/// words as a peer's -- is a pure function a test can hold, with no home directory and no environment
/// variable in the way. `own` holds the lines this run wrote (see `Mailbox::note_own`); a match is
/// consumed, so two runs saying the same sentence do not cancel each other out.
fn peer_messages(fresh: &str, me: &str, own: &mut Vec<String>) -> Vec<PeerMessage> {
    let mut out = Vec::new();
    for line in fresh.lines() {
        if let Some(mine) = own.iter().position(|written| written.trim_end() == line) {
            own.remove(mine);
            continue;
        }
        if let Some(message) = parse_peer(line, me) {
            out.push(message);
        }
    }
    out
}

/// One mailbox line as a message, or `None` when it is not for this run.
fn parse_peer(line: &str, me: &str) -> Option<PeerMessage> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Some(PeerMessage {
            from: "an unreadable line".to_string(),
            text: line.to_string(),
            to: String::new(),
            at: 0,
        });
    };
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let to = field("to");
    if !to.is_empty() && to != me {
        return None;
    }
    let text = value
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if text.trim().is_empty() {
        return None;
    }
    Some(PeerMessage {
        from: field("from"),
        text,
        to,
        at: value.get("at").and_then(|v| v.as_u64()).unwrap_or(0),
    })
}

/// The one sentence that says what the "changed" section does and does not know.
///
/// A pure function because its three cases *are* the honesty of the whole signal, and they are easy to
/// collapse into a single wrong one: "nothing changed", "something changed but not recently" and "here
/// is what changed recently" are three different statements, and the first may only be said when git
/// reported nothing at all. Written here rather than assembled at the print site so a test can hold
/// each case still -- which it does, after a real run in a real checkout printed "nothing changed"
/// while three uncommitted files sat in the directory, merely older than the window.
pub fn changed_line(git_paths: usize, shown: usize, window_minutes: u64) -> String {
    if git_paths == 0 {
        return "nothing that git tracks has changed".to_string();
    }
    if shown == 0 {
        return format!(
            "{git_paths} tracked path{} changed, but none written in the last {window_minutes} minutes",
            if git_paths == 1 { " has" } else { "s have" }
        );
    }
    let mut line = format!(
        "{shown} path{} written in the last {window_minutes} minutes",
        if shown == 1 { "" } else { "s" }
    );
    if git_paths > shown {
        line.push_str(&format!(
            " ({} more tracked path{} changed but older)",
            git_paths - shown,
            if git_paths - shown == 1 { " has" } else { "s have" }
        ));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn who_will_hear_it_is_said_three_ways_and_none_of_them_promises_a_listener() {
        let record = |pid: u32| Presence {
            pid,
            cwd: PathBuf::from("/tmp"),
            provider: "p".into(),
            model: "m".into(),
            readonly: false,
            started: now_secs(),
            last_seen: now_secs(),
            session: String::new(),
            nonce: format!("n{pid}"),
        };
        // Nobody: not a failure, and it must not read like one -- a mailbox is a file, and the message
        // is waiting in it for the next run rather than lost.
        let none = audience(&[]);
        assert!(none.contains("nobody else is working here"), "{none}");
        assert!(none.contains("waits in the file"), "{none}");
        // One: named by pid, because that is what `--to` takes.
        let one = audience(&[record(4242)]);
        assert!(one.contains("pid 4242"), "{one}");
        // Several: counted, and all of them named rather than summarised away.
        let many = audience(&[record(11), record(22)]);
        assert!(many.starts_with("2 runs are working here"), "{many}");
        assert!(many.contains("11, 22"), "{many}");
    }

    #[test]
    fn a_mailbox_line_this_run_wrote_is_not_a_peers_word() {
        // The reader is handed back exactly what the writer appended, and drops one match per note --
        // so a peer's identical sentence is still shown rather than swallowed along with it.
        let mine = "{\"from\":\"pid 1\",\"to\":\"\",\"at\":1,\"text\":\"I am editing src/provider.rs\"}";
        let theirs = "{\"from\":\"pid 2\",\"to\":\"\",\"at\":2,\"text\":\"the tree is yours\"}";
        let mut own = vec![format!("{mine}\n")];
        let read = |own: &mut Vec<String>, lines: &str| -> Vec<String> {
            peer_messages(lines, "1", own)
                .into_iter()
                .map(|message| message.text)
                .collect()
        };
        assert_eq!(
            read(&mut own, &format!("{mine}\n{theirs}\n")),
            vec!["the tree is yours".to_string()],
            "the run was shown its own words, or lost the peer's"
        );
        // The note is spent by the read. The same sentence arriving again is somebody else's message,
        // which is the case that would be lost if the writer were recognised by its `from` field.
        assert_eq!(
            read(&mut own, &format!("{mine}\n")),
            vec!["I am editing src/provider.rs".to_string()]
        );
    }

    #[test]
    fn what_changed_is_never_confused_with_whether_anything_was_looked_at() {
        // Nothing changed at all: the only case that may say so.
        assert_eq!(
            changed_line(0, 0, 10),
            "nothing that git tracks has changed"
        );
        // Changed, but older than the window: "nothing changed" would be a lie about work in
        // progress, which is exactly what an uncommitted revision is.
        let older = changed_line(3, 0, 10);
        assert_eq!(
            older,
            "3 tracked paths have changed, but none written in the last 10 minutes"
        );
        // Recent changes, with the rest counted rather than dropped.
        let some = changed_line(7, 2, 10);
        assert!(some.starts_with("2 paths written in the last 10 minutes"), "{some}");
        assert!(some.contains("5 more"), "{some}");
        // Singular and plural are both readable, because this line is read by a person.
        assert_eq!(
            changed_line(1, 0, 10),
            "1 tracked path has changed, but none written in the last 10 minutes"
        );
        assert_eq!(changed_line(1, 1, 10), "1 path written in the last 10 minutes");
    }

    #[test]
    fn a_fresh_record_is_alive_and_an_old_one_is_stale() {
        let mut record = Presence {
            pid: 1,
            cwd: PathBuf::from("/tmp"),
            provider: "p".into(),
            model: "m".into(),
            readonly: false,
            started: now_secs(),
            last_seen: now_secs(),
            session: String::new(),
            nonce: "n".into(),
        };
        assert!(!record.is_stale());
        // The boundary is the one that matters: a run that has just missed a beat is alive.
        record.last_seen = now_secs() - STALE_AFTER.as_secs();
        assert!(!record.is_stale());
        record.last_seen = now_secs() - STALE_AFTER.as_secs() - 1;
        assert!(record.is_stale());
    }

    #[test]
    fn what_arrives_from_a_hand_edited_record_is_reported_rather_than_guessed() {
        // A record with no `last_seen` cannot answer the only question asked of it, so it is damage.
        assert!(serde_json::from_str::<Presence>("{\"pid\": 1}").is_err());
    }

    /// Wait a bounded moment for what the test just wrote to be readable again.
    ///
    /// Added for a race that turned out not to exist: the failure it was written for was
    /// `project_dir_until` resolving one directory two ways, so that `say` wrote to one mailbox and
    /// the reader followed another (that function's comment has the mechanism, and the commit after
    /// the one that added this says so). It is kept for two reasons. It costs a read and a compare on
    /// any machine that behaves, and it buys a failure message that carries the whole file rather
    /// than one line of an assertion -- which is what turned that wrong diagnosis into the right one
    /// in a single CI run. And the claim it removes was never this test's to make anyway: a fresh
    /// read immediately after a write and a close is not something a file system guarantees.
    fn visible(path: &Path, ready: impl Fn(&str) -> bool) -> String {
        let mut text = String::new();
        for _ in 0..100 {
            text = std::fs::read_to_string(path).unwrap_or_default();
            if ready(&text) {
                return text;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        text
    }

    #[test]
    fn a_mailbox_shows_what_arrives_after_it_started_and_only_what_is_for_this_run() {
        // A directory of its own for the mailbox, and a *project* directory of its own to key it by.
        // The mailbox itself lives under `FLINT_HOME` (whatever this test process was given), so the
        // key is a path nothing else uses and the file is removed at the end.
        let dir = std::env::temp_dir().join(format!("flint-mailbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cwd = dir.join("project");
        std::fs::create_dir_all(&cwd).expect("project dir");
        let path = mailbox_path(&cwd);
        let _ = std::fs::remove_file(&path);

        // Everything already there when a run starts is history, not news: a run that opened onto a
        // full mailbox and replayed it would report yesterday's conversation as happening now.
        say(&cwd, "old", "", "said before this run").expect("write");
        let mut mailbox = Mailbox::following(&cwd);
        // The same directory asked twice is the same mailbox -- not a tautology, and not a test of
        // the marker: a path that moves partway through a process is a message written to one file
        // and read from another. It did move, once, on a Windows CI runner, and this is the line
        // that would have said so in one run instead of two (`project_dir_until`'s comment has the
        // mechanism, which is about how that machine spells its own home directory).
        assert_eq!(mailbox.path, path, "the reader follows a different mailbox");
        assert_eq!(
            mailbox_path(&cwd),
            path,
            "the mailbox for this directory moved after the first write"
        );
        assert!(mailbox.new_messages("1").is_empty());

        say(&cwd, "peer 2", "", "said to whoever is here").expect("write");
        say(&cwd, "peer 3", "1", "said to me").expect("write");
        say(&cwd, "peer 4", "999", "said to somebody else").expect("write");
        let landed = visible(&path, |text| text.contains("said to somebody else"));
        let mine = mailbox.new_messages("1");
        assert_eq!(
            mine.len(),
            2,
            "expected the broadcast and the one to me: {mine:?} in {landed:?}"
        );
        assert_eq!(mine[0].from, "peer 2");
        assert_eq!(mine[1].text, "said to me");
        // Read once, not twice: the cursor is the whole reason a run does not repeat itself.
        assert!(mailbox.new_messages("1").is_empty());

        // A half-written line is not a message: the rest of it may not exist yet, and showing it would
        // be showing words nobody sent. It is kept for the next read.
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open");
            file.write_all(br#"{"from":"peer 5","text":"not finishe"#)
                .expect("partial write");
        }
        let half = visible(&path, |text| text.ends_with("not finishe"));
        assert!(
            mailbox.new_messages("1").is_empty(),
            "half a line was read as a message: {half:?}"
        );
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open");
            file.write_all(b"d yet\"}\n").expect("finish the line");
        }
        let finished_line = visible(&path, |text| text.ends_with("not finished yet\"}\n"));
        let finished = mailbox.new_messages("1");
        assert_eq!(
            finished.len(),
            1,
            "the completed line was not read: {finished:?} in {finished_line:?}"
        );
        assert_eq!(finished[0].text, "not finished yet");

        // A line somebody broke by hand is reported as a peer message that says so: two agents failing
        // to talk because of one bad line is the silence this exists to break.
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open");
            file.write_all(b"not json at all\n").expect("bad line");
        }
        visible(&path, |text| text.ends_with("not json at all\n"));
        let damaged = mailbox.new_messages("1");
        assert_eq!(damaged.len(), 1);
        assert!(damaged[0].from.contains("unreadable"), "{damaged:?}");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_says_how_long_ago_it_was_seen_in_words() {
        let record = Presence {
            pid: 1,
            cwd: PathBuf::from("/tmp"),
            provider: "p".into(),
            model: "m".into(),
            readonly: false,
            started: now_secs() - 3700,
            last_seen: now_secs() - 3,
            session: String::new(),
            nonce: "n".into(),
        };
        assert_eq!(record.seen_ago(), "3s ago");
        assert_eq!(record.age(), "1h");
    }

    /// A record from a build that had no session, or one a person wrote by hand, still reads.
    ///
    /// The alternative -- a required field -- would turn every existing record into damage on the day
    /// this shipped, and "damage" is what the reader reports to a person as something to look at.
    #[test]
    fn a_record_without_a_session_is_a_record_with_no_session() {
        let old = r#"{"pid":1,"cwd":"/tmp","provider":"p","model":"m","readonly":false,
                      "started":1,"last_seen":2}"#;
        let record: Presence = serde_json::from_str(old).expect("an older record still reads");
        assert_eq!(record.session, "");
    }
}
