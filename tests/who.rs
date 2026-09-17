//! `flint who`: who else is working here, and what changed that nobody will own up to.
//!
//! The command exists because of an accident recorded in `ROADMAP.md` -- two agents in one checkout,
//! one of them mid-write, and the other committing a half-finished file. So the tests are about the
//! properties that would have prevented it, and about the honesty of the answer: a record that
//! disappears when its writer does, a record nobody cleaned up being called *stale* rather than
//! alive, damage being reported rather than skipped, and a "changed recently" line that never claims
//! to know who changed anything.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn cwd_for(tag: &str) -> PathBuf {
    let cwd = std::env::temp_dir().join(format!("flint-who-cwd-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).expect("working directory");
    cwd
}

/// A home with one provider pointing at `base`, and the key set unless the test says otherwise.
fn home_for(tag: &str, base: &str, key: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("flint-who-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(
        home.join("config.toml"),
        format!(
            "default_provider = \"stub\"\n\
             \n[[providers]]\n\
             name = \"stub\"\n\
             base_url = \"{base}/v1\"\n\
             model = \"stub-model\"\n\
             api_key = \"{key}\"\n"
        ),
    )
    .expect("config");
    home
}

fn run_who(home: &Path, cwd: &Path, extra: &[&str]) -> (String, i32) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_flint"));
    command.arg("who").arg("--json").arg("--cwd").arg(cwd);
    for arg in extra {
        command.arg(arg);
    }
    let out = command
        .env("FLINT_HOME", home)
        .output()
        .expect("run flint who");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn json_of(stdout: &str) -> Value {
    let line = stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_else(|| panic!("no output at all: {stdout:?}"));
    serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line:?}"))
}

/// Every record file in a home, alive or not. The name carries a nonce, so it cannot be predicted.
fn records(home: &Path) -> Vec<PathBuf> {
    let dir = home.join("live");
    match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A record written by hand, the way a killed process leaves one behind.
fn write_record(home: &Path, name: &str, cwd: &Path, last_seen: u64, readonly: bool) {
    std::fs::create_dir_all(home.join("live")).expect("live dir");
    let body = serde_json::json!({
        "pid": 4242,
        "cwd": cwd.display().to_string(),
        "provider": "stub",
        "model": "stub-model",
        "readonly": readonly,
        "started": last_seen - 30,
        "last_seen": last_seen,
    });
    std::fs::write(home.join("live").join(name), body.to_string()).expect("record");
}

/// A run that says something, slowly enough to be observed while it is still working.
struct Slow(String);

impl Respond for Slow {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_delay(Duration::from_secs(30))
            .set_body_string(self.0.clone())
    }
}

fn one_fragment() -> String {
    concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"working\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string()
}

/// The property the whole command rests on: a run that is working says so, and a run that has ended
/// takes its record with it.
///
/// Both halves in one test on purpose. A record that appears but is never removed would make `who`
/// useless in the other direction -- every run ever started would still be listed -- and the second
/// half is the one that is easy to leave out, because it is the one that only happens on the way out.
#[tokio::test]
async fn a_run_says_it_is_here_and_stops_saying_it_when_it_ends() {
    let server = MockServer::start().await;
    let cwd = cwd_for("live");
    let home = home_for("live", &server.uri(), "not-a-real-key");
    Mock::given(method("POST"))
        .respond_with(Slow(one_fragment()))
        .mount(&server)
        .await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["-p", "say hello", "--json"])
        .arg("--cwd")
        .arg(&cwd)
        .env("FLINT_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn flint");

    // Wait for the record rather than sleeping a fixed amount: the run writes it as soon as it has a
    // provider, and the wait is what makes this test about the record rather than about the timing.
    let mut found = false;
    for _ in 0..100 {
        if !records(&home).is_empty() {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(found, "the live run never announced itself");

    let (stdout, code) = run_who(&home, &cwd, &[]);
    assert_eq!(code, 0, "who failed: {stdout}");
    let answer = json_of(&stdout);
    let live = answer["live"].as_array().expect("live");
    assert_eq!(live.len(), 1, "expected one live run: {answer}");
    assert_eq!(live[0]["provider"], "stub");
    assert_eq!(live[0]["model"], "stub-model");
    assert_eq!(live[0]["readonly"], false);
    assert_eq!(
        live[0]["cwd"].as_str().unwrap(),
        cwd.display().to_string(),
        "the record names the directory it belongs to"
    );
    assert!(
        live[0]["seen_secs_ago"].as_u64().unwrap() < 60,
        "a run that is working must not look stale: {answer}"
    );

    // `/stop` rather than a kill, so the run ends the way a person ends one -- and so the record is
    // removed on the way out rather than left for the staleness window to explain.
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"/stop\n").expect("stop");
        stdin.flush().expect("flush");
    }
    let _ = child.wait();

    let mut gone = false;
    for _ in 0..100 {
        if records(&home).is_empty() {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        gone,
        "the run ended but its record is still there: {:?}",
        records(&home)
    );
}

/// A project that keeps a `.flint/` directory is a project whose runs can be seen *across
/// installations*, which is the one hole the presence record could not close by itself.
///
/// The home's record stays the one that must exist -- a `readonly` run has to be able to announce
/// itself and a checkout is not always writable -- so the project's copy has exactly one job: it is
/// the only thing a flint with a *different* `FLINT_HOME` can see. That is what this asserts, with a
/// second, empty home doing the asking, and it asserts the other half too: the copy is removed on the
/// way out, like the record it copies.
#[tokio::test]
async fn a_project_marker_lets_another_installation_see_a_run() {
    let server = MockServer::start().await;
    let cwd = cwd_for("marker");
    std::fs::create_dir_all(cwd.join(".flint")).expect("the marker");
    let home = home_for("marker-here", &server.uri(), "not-a-real-key");
    // A second installation: its own home, nothing in it, and no way to see the first one's run
    // except through the project.
    let stranger = home_for("marker-there", &server.uri(), "not-a-real-key");
    Mock::given(method("POST"))
        .respond_with(Slow(one_fragment()))
        .mount(&server)
        .await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["-p", "say hello", "--json"])
        .arg("--cwd")
        .arg(&cwd)
        .env("FLINT_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn flint");

    let project_records = || records(&cwd.join(".flint"));
    let mut found = false;
    for _ in 0..100 {
        if !project_records().is_empty() {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        found,
        "a run in a marked project did not write into {}",
        cwd.join(".flint/live").display()
    );

    // The whole point: asked by the *other* installation, about this directory.
    let (stdout, code) = run_who(&stranger, &cwd, &[]);
    assert_eq!(code, 0, "who failed: {stdout}");
    let answer = json_of(&stdout);
    let live = answer["live"].as_array().expect("live");
    assert_eq!(
        live.len(),
        1,
        "the second installation cannot see the run: {answer}"
    );
    assert_eq!(live[0]["provider"], "stub");
    assert_eq!(live[0]["cwd"].as_str().unwrap(), cwd.display().to_string());

    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"/stop\n").expect("stop");
        stdin.flush().expect("flush");
    }
    let _ = child.wait();

    let mut gone = false;
    for _ in 0..100 {
        if project_records().is_empty() && records(&home).is_empty() {
            gone = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        gone,
        "the run ended but left records behind: project {:?}, home {:?}",
        project_records(),
        records(&home)
    );
}

/// A checkout that never asked for flint's project state does not get any.
///
/// This is the promise that makes writing a copy into the project acceptable at all: `.flint/` is
/// created by a person -- a skill, a profile, or a bare `mkdir .flint` -- and never by flint. A run
/// in a stranger's tree therefore leaves exactly what it left before, which is what makes the marker
/// safe to look for on every run.
#[tokio::test]
async fn a_run_leaves_no_project_state_where_the_project_did_not_ask() {
    let server = MockServer::start().await;
    let cwd = cwd_for("nomarker");
    let home = home_for("nomarker", &server.uri(), "not-a-real-key");
    Mock::given(method("POST"))
        .respond_with(Slow(one_fragment()))
        .mount(&server)
        .await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["-p", "say hello", "--json"])
        .arg("--cwd")
        .arg(&cwd)
        .env("FLINT_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn flint");

    let mut found = false;
    for _ in 0..100 {
        if !records(&home).is_empty() {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(found, "the live run never announced itself in its home");
    assert!(
        !cwd.join(".flint").exists(),
        "flint created {} in a checkout that never asked for flint state",
        cwd.join(".flint").display()
    );

    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"/stop\n").expect("stop");
        stdin.flush().expect("flush");
    }
    let _ = child.wait();
    assert!(
        !cwd.join(".flint").exists(),
        "flint left project state behind in a checkout that never asked for it"
    );
}

/// One run writes one record into two directories, and it is one run, not two.
///
/// The name is the identity -- pid plus nonce, the same in both places -- and the later word about a
/// run is the one believed, because a pair of records where one was refreshed and the other was not
/// is a half-written pair rather than two processes. Without this, a run in a marked project would be
/// listed twice by `flint who`, which is the sort of wrong answer that makes a person stop trusting
/// the command.
#[test]
fn one_record_in_two_places_is_still_one_run() {
    let cwd = cwd_for("dedup");
    std::fs::create_dir_all(cwd.join(".flint")).expect("the marker");
    let home = home_for("dedup", "http://127.0.0.1:1/v1", "not-a-real-key");
    let now = now_secs();
    write_record(&home, "4242-samenonce.json", &cwd, now, false);
    write_record(&cwd.join(".flint"), "4242-samenonce.json", &cwd, now + 5, false);
    // A different run: its own nonce, so it is a second entry rather than a duplicate.
    write_record(&cwd.join(".flint"), "4242-othernonce.json", &cwd, now, false);

    let (stdout, code) = run_who(&home, &cwd, &[]);
    assert_eq!(code, 0, "who failed: {stdout}");
    let answer = json_of(&stdout);
    let live = answer["live"].as_array().expect("live");
    assert_eq!(
        live.len(),
        2,
        "two runs, each written once to the home and once to the project: {answer}"
    );
    let deduped = live
        .iter()
        .find(|record| record["last_seen"] == serde_json::json!(now + 5))
        .unwrap_or_else(|| panic!("the fresher copy of the shared record was not the one kept: {answer}"));
    assert_eq!(
        deduped["pid"], 4242,
        "the record that won is the run that wrote both copies"
    );
}

/// Every conversation file in a home, whichever working directory keyed it.
fn session_files(home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(dirs) = std::fs::read_dir(home.join("sessions")) else {
        return found;
    };
    for dir in dirs.flatten() {
        let Ok(entries) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// A live run says *which conversation* it is holding, not only that it is here.
///
/// The stage-2 note in `docs/agents.md` recorded this as the gap, and it is the gap that matters for a
/// parent watching a child: the record named the provider, the model and the flags, and `who` answered
/// "which conversation is live" by reporting the newest session file in the directory -- a guess that
/// is wrong exactly when it matters, with two runs writing at once, and it says nothing at all about a
/// run whose directory is not yours. The session travels in the record, so a person reading `who` can
/// point at the conversation a run is holding while it is still working, which is also what a handle
/// for a background child is made of.
#[tokio::test]
async fn a_live_run_names_the_conversation_it_is_holding() {
    let server = MockServer::start().await;
    let cwd = cwd_for("session");
    let home = home_for("session", &server.uri(), "not-a-real-key");
    Mock::given(method("POST"))
        .respond_with(Slow(one_fragment()))
        .mount(&server)
        .await;

    let mut child = Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(["-p", "say hello", "--json"])
        .arg("--cwd")
        .arg(&cwd)
        .env("FLINT_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn flint");

    // The conversation is created by the first thing said, which is the prompt itself, so this waits
    // for the file rather than for the model: the model is deliberately still answering.
    let mut written = Vec::new();
    for _ in 0..100 {
        written = session_files(&home);
        if !written.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(written.len(), 1, "the run never wrote its conversation: {written:?}");
    let session = written[0].display().to_string();

    let (stdout, code) = run_who(&home, &cwd, &[]);
    assert_eq!(code, 0, "who failed: {stdout}");
    let answer = json_of(&stdout);
    let live = answer["live"].as_array().expect("live");
    assert_eq!(live.len(), 1, "expected one live run: {answer}");
    assert_eq!(
        live[0]["session"].as_str().unwrap_or_default(),
        session,
        "the record must name the file this run is writing, not the newest one it can find: {answer}"
    );
    // A record written by a hand or by an older build has no session, and that is reported as "not
    // known" rather than as an empty file name: `null`, like every other absent field here.
    assert!(
        !live[0]["session"].is_null(),
        "a run holding a conversation reported none: {answer}"
    );

    // The human line names it too, because a person reading `who` is the reader who cannot look up a
    // JSON field -- and the id is what they would type at `--resume`.
    let human = Command::new(env!("CARGO_BIN_EXE_flint"))
        .arg("who")
        .arg("--cwd")
        .arg(&cwd)
        .env("FLINT_HOME", &home)
        .output()
        .expect("run flint who");
    let text = String::from_utf8_lossy(&human.stdout);
    let stem = written[0]
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    assert!(text.contains(&stem), "the human listing must name it too: {text}");

    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"/stop\n").expect("stop");
        stdin.flush().expect("flush");
    }
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&home);
}

/// A record nobody cleaned up is *stale*, and is never listed as a live run.
///
/// This is the case a killed process leaves, and it is the reason nothing here depends on asking the
/// operating system whether a pid exists: the answer would differ per machine, and a wrong "alive" is
/// exactly as bad as a wrong "dead".
#[test]
fn a_record_nobody_cleaned_up_is_stale_and_not_alive() {
    let cwd = cwd_for("stale");
    let home = home_for("stale", "http://127.0.0.1:1", "not-a-real-key");
    // Ten minutes and more: far past the window, and the same shape a `kill -9` leaves behind.
    write_record(&home, "4242-1.json", &cwd, now_secs() - 700, true);

    let (stdout, code) = run_who(&home, &cwd, &[]);
    assert_eq!(code, 0, "who failed: {stdout}");
    let answer = json_of(&stdout);
    assert!(
        answer["live"].as_array().expect("live").is_empty(),
        "a ten-minute-old record is not a live run: {answer}"
    );
    let stale = answer["stale"].as_array().expect("stale");
    assert_eq!(stale.len(), 1, "expected one stale record: {answer}");
    assert_eq!(stale[0]["pid"], 4242);
    assert_eq!(stale[0]["readonly"], true);
    assert!(
        stale[0]["seen_secs_ago"].as_u64().unwrap() > 600,
        "the age is reported rather than guessed: {answer}"
    );
}

/// A record that cannot be read is reported, not skipped.
///
/// Silence would look exactly like "no other agent", which is the one answer this command must never
/// get wrong by accident. A person who hand-edited a record into nonsense wants to be told.
#[test]
fn a_record_that_cannot_be_read_is_reported_rather_than_ignored() {
    let cwd = cwd_for("damaged");
    let home = home_for("damaged", "http://127.0.0.1:1", "not-a-real-key");
    std::fs::create_dir_all(home.join("live")).expect("live dir");
    std::fs::write(home.join("live").join("nonsense.json"), "{ this is not json").expect("write");

    let (stdout, code) = run_who(&home, &cwd, &[]);
    assert_eq!(code, 0, "who failed: {stdout}");
    let answer = json_of(&stdout);
    let unreadable = answer["unreadable"].as_array().expect("unreadable");
    assert_eq!(unreadable.len(), 1, "expected the damage to be named: {answer}");
    assert!(
        unreadable[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("nonsense.json"),
        "{answer}"
    );
}

/// `who` needs no key, and does not count itself as a run.
///
/// It is asked before running anything -- often on the machine where the provider is what is in
/// doubt -- so it must work with an empty `api_key`. And it is a question, not work: a `who` that
/// appeared in its own answer would be noise in every listing.
#[test]
fn who_answers_without_a_key_and_does_not_list_itself() {
    let cwd = cwd_for("nokey");
    let home = home_for("nokey", "http://127.0.0.1:1", "");

    let (stdout, code) = run_who(&home, &cwd, &[]);
    assert_eq!(code, 0, "who with no key: {stdout}");
    let answer = json_of(&stdout);
    assert_eq!(answer["type"], "who");
    assert!(
        answer["live"].as_array().expect("live").is_empty(),
        "a question is not a run: {answer}"
    );
    assert!(
        records(&home).is_empty(),
        "`who` wrote a presence record for itself: {:?}",
        records(&home)
    );
}

/// The line about changed files names files, never an author.
///
/// The other agent in the incident was probably not flint at all, so this is the only signal that
/// covers it -- and it is the signal that cannot know who did anything. Both outputs say so.
#[test]
fn what_changed_is_reported_without_claiming_to_know_who_changed_it() {
    let cwd = cwd_for("changed");
    let home = home_for("changed", "http://127.0.0.1:1", "not-a-real-key");
    std::fs::write(cwd.join("something.txt"), "just written").expect("write");

    let (stdout, code) = run_who(&home, &cwd, &[]);
    assert_eq!(code, 0, "who failed: {stdout}");
    let answer = json_of(&stdout);
    let note = answer["note"].as_str().expect("a note about what this cannot see");
    assert!(
        note.contains("names no author"),
        "the machine-readable answer must say the same thing: {note}"
    );
    assert!(
        answer["changed"]["files"].is_array(),
        "the changed files are listed as files: {answer}"
    );
    // And nothing in the answer claims to know who: the only author named anywhere is flint itself,
    // in the list of live runs, which is empty here.
    assert!(
        answer["live"].as_array().unwrap().is_empty(),
        "{answer}"
    );

    // The human output says it in words, because a person reading it is the one who would draw the
    // wrong conclusion -- "nothing here" is not "nobody here".
    let human = Command::new(env!("CARGO_BIN_EXE_flint"))
        .arg("who")
        .arg("--cwd")
        .arg(&cwd)
        .env("FLINT_HOME", &home)
        .output()
        .expect("run flint who");
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(
        text.contains("this names no author"),
        "the human line must say it too: {text}"
    );
    assert!(text.contains("none that flint can see"), "{text}");
}

/// `--all` is how a run in another directory is named.
///
/// Off by default because the question is about *here* -- a collision happens in one directory -- and
/// the rest of the machine is noise until it is asked for.
#[test]
fn a_run_elsewhere_is_counted_and_named_only_when_asked_for() {
    let cwd = cwd_for("all-here");
    let elsewhere = cwd_for("all-there");
    let home = home_for("all", "http://127.0.0.1:1", "not-a-real-key");
    write_record(&home, "4242-1.json", &elsewhere, now_secs(), false);

    let (stdout, _) = run_who(&home, &cwd, &[]);
    let answer = json_of(&stdout);
    assert_eq!(answer["other_live"], 1, "{answer}");
    assert!(
        answer["live"].as_array().unwrap().is_empty(),
        "a run in another directory is not in this one: {answer}"
    );
    assert!(
        answer.get("live_elsewhere").is_none(),
        "names are not listed unless asked for: {answer}"
    );

    let (stdout, _) = run_who(&home, &cwd, &["--all"]);
    let answer = json_of(&stdout);
    let named = answer["live_elsewhere"].as_array().expect("live_elsewhere");
    assert_eq!(named.len(), 1, "{answer}");
    assert_eq!(
        named[0]["cwd"].as_str().unwrap(),
        elsewhere.display().to_string()
    );
}
