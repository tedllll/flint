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
