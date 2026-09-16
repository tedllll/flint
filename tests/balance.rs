//! `flint balance`: the preflight, against a stub provider.
//!
//! The command's whole reason to exist is being trusted *before* a batch spends money, so what these
//! tests pin is mostly the honesty of the answer: the money question is asked only where it can be
//! answered, an endpoint that decides nothing says so instead of saying "usable", and the exit codes
//! are the ones a run that failed would give -- a batch can branch on either.
//!
//! A real key is never used: the stub is `wiremock`, and the key in the config is the word "test".
//! Same shape as `json_output.rs`, which is where the vocabulary these codes belong to is tested.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const AVAILABLE: &str = r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"110.00","granted_balance":"10.00","topped_up_balance":"100.00"}]}"#;
const EMPTY: &str = r#"{"is_available":false,"balance_infos":[{"currency":"CNY","total_balance":"0.00","granted_balance":"0.00","topped_up_balance":"0.00"}]}"#;

/// A working directory that exists, because `--cwd` refuses one that does not.
fn cwd_for(tag: &str) -> PathBuf {
    let cwd = std::env::temp_dir().join(format!("flint-balance-cwd-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).expect("working directory");
    cwd
}

/// A home with one provider, pointed at `base`, and the key the tests use.
fn home_for(tag: &str, base: &str, name: &str, key: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("flint-balance-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("home directory");
    std::fs::write(
        home.join("config.toml"),
        format!(
            "default_provider = \"{name}\"\n\n[[providers]]\nname = \"{name}\"\nbase_url = \"{base}\"\napi_key = \"{key}\"\nmodel = \"stub-model\"\n"
        ),
    )
    .expect("config");
    home
}

/// Run the real binary's preflight and return (exit code, stdout, stderr).
fn run_balance(home: &Path, cwd: &Path, extra: &[&str]) -> (i32, String, String) {
    let mut args = vec!["balance"];
    args.extend_from_slice(extra);
    let out = Command::new(env!("CARGO_BIN_EXE_flint"))
        .args(&args)
        .arg("--cwd")
        .arg(cwd)
        .env("FLINT_HOME", home)
        .output()
        .expect("failed to run flint");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// The one JSON object `--json` promises, parsed strictly: a second line would be a broken contract.
fn json_of(stdout: &str) -> Value {
    let mut lines = stdout.lines();
    let first = lines.next().unwrap_or_default();
    assert!(
        lines.next().is_none(),
        "the preflight wrote more than one object: {stdout:?}"
    );
    serde_json::from_str(first).unwrap_or_else(|e| panic!("not one JSON object: {first:?} ({e})"))
}

/// DeepSeek is the provider whose balance can be asked for, and the answer is the whole point.
#[tokio::test]
async fn a_deepseek_account_is_asked_for_its_balance() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_string(AVAILABLE))
        .expect(1)
        .mount(&server)
        .await;
    let cwd = cwd_for("deepseek");
    let base = format!("{}/v1", server.uri());
    let home = home_for("deepseek", &base, "deepseek", "test");

    let (code, stdout, stderr) = run_balance(&home, &cwd, &["--json"]);
    let _ = std::fs::remove_dir_all(&home);

    let value = json_of(&stdout);
    assert_eq!(code, 0, "a usable provider is not a failure: {stderr} {stdout}");
    assert_eq!(value["checked"], "balance", "{stdout}");
    assert_eq!(value["usable"], true, "{stdout}");
    assert_eq!(value["total_balance"], "110.00", "{stdout}");
    assert_eq!(value["currency"], "CNY", "{stdout}");
    assert_eq!(value["granted_balance"], "10.00", "{stdout}");
    assert_eq!(value["topped_up_balance"], "100.00", "{stdout}");
    // The URL matters: a `base_url` that already ends in `/v1` must not grow a second one, and the
    // balance endpoint lives beside `/chat/completions` rather than under it.
    let _ = cwd;
}

/// An account with nothing in it, said before a batch starts rather than on its hundredth call.
#[tokio::test]
async fn an_empty_account_is_unavailable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_string(EMPTY))
        .mount(&server)
        .await;
    let cwd = cwd_for("empty");
    let base = format!("{}/v1", server.uri());
    let home = home_for("empty", &base, "deepseek", "test");

    let (code, stdout, stderr) = run_balance(&home, &cwd, &["--json"]);
    let _ = std::fs::remove_dir_all(&home);

    let value = json_of(&stdout);
    assert_eq!(value["usable"], false, "{stdout}");
    assert_eq!(code, 69, "nothing in the account is a person's job: {stderr} {stdout}");
    assert_eq!(value["total_balance"], "0.00", "{stdout}");
}

/// The provider refusing to answer the balance question is the same failure a run would report, and it
/// has to arrive with the same code -- otherwise a batch has two vocabularies to check.
#[tokio::test]
async fn a_rejected_balance_request_arrives_classified() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user/balance"))
        .respond_with(ResponseTemplate::new(402).set_body_string(r#"{"error":{"message":"Insufficient Balance"}}"#))
        .mount(&server)
        .await;
    let cwd = cwd_for("402");
    let base = format!("{}/v1", server.uri());
    let home = home_for("402", &base, "deepseek", "test");

    let (code, stdout, _) = run_balance(&home, &cwd, &["--json"]);
    let _ = std::fs::remove_dir_all(&home);

    let value = json_of(&stdout);
    assert_eq!(value["type"], "error", "{stdout}");
    assert_eq!(value["code"], "insufficient_balance", "{stdout}");
    assert_eq!(value["retryable"], false, "{stdout}");
    assert_eq!(code, 69, "{stdout}");
}

/// A provider with no balance API is asked what it *can* answer, and the answer says which question was
/// asked: "usable" from a `/models` probe is not the same statement as "usable, 110 CNY left".
#[tokio::test]
async fn another_provider_is_asked_only_what_it_publishes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"data":[{"id":"stub-model"}]}"#))
        .expect(1)
        .mount(&server)
        .await;
    let cwd = cwd_for("models");
    let base = format!("{}/v1", server.uri());
    let home = home_for("models", &base, "somewhereelse", "test");

    let (code, stdout, stderr) = run_balance(&home, &cwd, &["--json"]);
    let _ = std::fs::remove_dir_all(&home);

    let value = json_of(&stdout);
    assert_eq!(value["checked"], "models", "{stdout}");
    assert_eq!(value["usable"], true, "{stdout}");
    assert!(
        value.get("total_balance").is_none(),
        "a balance that was never asked for must be absent, not null: {stdout}"
    );
    assert_eq!(code, 0, "{stderr} {stdout}");
}

/// Reached, and answered nothing flint can use: a local engine that serves only `/chat/completions`.
///
/// This is the answer that must not be "usable". Nothing was checked -- not the key, not the account --
/// and a preflight reporting a verdict it did not establish is worse than reporting none.
#[tokio::test]
async fn an_endpoint_that_decides_nothing_says_so() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let cwd = cwd_for("none");
    let base = format!("{}/v1", server.uri());
    let home = home_for("none", &base, "localish", "test");

    let (code, stdout, _) = run_balance(&home, &cwd, &["--json"]);
    let _ = std::fs::remove_dir_all(&home);

    let value = json_of(&stdout);
    assert_eq!(value["checked"], "none", "{stdout}");
    assert_eq!(value["usable"], Value::Null, "{stdout}");
    assert_eq!(code, 1, "1 means unclassified, which is the honest code here: {stdout}");
}

/// No key at all, which is the cheapest thing a preflight can find and the most useful.
#[tokio::test]
async fn a_missing_key_is_named() {
    let cwd = cwd_for("nokey");
    let home = home_for("nokey", "https://api.deepseek.com/v1", "deepseek", "");

    let (code, stdout, _) = run_balance(&home, &cwd, &["--json"]);
    let _ = std::fs::remove_dir_all(&home);

    let value = json_of(&stdout);
    assert_eq!(value["code"], "no_key", "{stdout}");
    assert_eq!(code, 69, "{stdout}");
}

/// The line a person reads names the provider and what was actually checked.
#[tokio::test]
async fn the_human_line_says_what_was_checked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_string(AVAILABLE))
        .mount(&server)
        .await;
    let cwd = cwd_for("human");
    let base = format!("{}/v1", server.uri());
    let home = home_for("human", &base, "deepseek", "test");

    let (code, stdout, stderr) = run_balance(&home, &cwd, &[]);
    let _ = std::fs::remove_dir_all(&home);

    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("deepseek"), "{stdout}");
    assert!(stdout.contains("110.00 CNY"), "{stdout}");
    assert!(
        !stdout.trim_start().starts_with('{'),
        "the human view is not JSON: {stdout}"
    );
}
