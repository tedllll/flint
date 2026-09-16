//! Who else is working in this directory.
//!
//! Two agents in one checkout is the accident this exists for: they collide over a file, or one
//! commits a revision the other is still writing, and neither can see it coming. `ROADMAP.md`'s
//! "being used by another agent" section records the incident that made it concrete. `docs/agents.md`
//! is the plan this is stage 1 of.
//!
//! It answers with two signals that must never be blended into one sentence, because they have
//! different authors and different precision:
//!
//! - a **presence record**, written by a running flint and refreshed while it lives. Exact — and
//!   blind to everything that is not flint.
//! - **recent changes** on disk (`git status`, file mtimes). True of every tool on the machine, and
//!   unable to name an author.
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

    fn file(&self) -> PathBuf {
        crate::config::live_dir().join(format!("{}-{}.json", self.pid, self.nonce))
    }
}

/// A record that is being kept alive, and removed when it is dropped.
///
/// Dropped rather than closed by hand, so that every exit path is covered by construction: an early
/// return, an error, a panic that unwinds. `kill -9` leaves the file behind, which is the case
/// [`Presence::is_stale`] exists for.
pub struct Guard {
    record: Presence,
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
            nonce,
        };
        if let Err(e) = write_record(&record) {
            // A run that cannot announce itself still runs. Saying so on stderr is the honest
            // version: the alternative is either a silent failure to appear in `flint who`, or
            // refusing to work because a directory could not be created.
            eprintln!("flint: warning: cannot write {}: {e}", record.file().display());
        }

        let (stop, wake) = std::sync::mpsc::channel::<()>();
        let thread = {
            let mut record = record.clone();
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
                        record.last_seen = now_secs();
                        let _ = write_record(&record);
                    }
                })
                .ok()
        };
        Guard {
            record,
            stop: Some(stop),
            thread,
        }
    }

    /// Where this run's record is, for a message that wants to name it.
    pub fn path(&self) -> PathBuf {
        self.record.file()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Dropping the sender closes the channel, which ends the thread's wait at once.
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(self.record.file());
    }
}

/// Write a record where a reader can never see half of one.
///
/// Through a temporary file and a rename, because `flint who` may read this at any moment and a
/// truncated JSON object would be reported as damage -- a warning the reader cannot act on, produced
/// by flint itself, is worse than the microseconds a rename costs.
fn write_record(record: &Presence) -> std::io::Result<()> {
    let dir = crate::config::live_dir();
    std::fs::create_dir_all(&dir)?;
    let path = record.file();
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

pub fn scan() -> Listing {
    let mut listing = Listing {
        alive: Vec::new(),
        stale: Vec::new(),
        unreadable: Vec::new(),
    };
    let dir = crate::config::live_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // No directory is the normal state of a machine where nothing has run yet.
        Err(_) => return listing,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&path).map(|t| serde_json::from_str::<Presence>(&t)) {
            Ok(Ok(record)) => {
                if record.is_stale() {
                    listing.stale.push(record);
                } else {
                    listing.alive.push(record);
                }
            }
            Ok(Err(e)) => listing.unreadable.push((path, e.to_string())),
            Err(e) => listing.unreadable.push((path, e.to_string())),
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
            nonce: "n".into(),
        };
        assert_eq!(record.seen_ago(), "3s ago");
        assert_eq!(record.age(), "1h");
    }
}
