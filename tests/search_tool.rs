//! The `search` tool, end to end against a stub of DeepSeek's search endpoint.
//!
//! `src/search.rs` tests the resolution rules and the parser; this is the part that says a
//! request actually leaves and comes back as an answer for the model. The stub replays a
//! response shaped like the one measured on 2026-09-13 (`docs/deepseek-search.md`), so the
//! thing under test is the real path rather than a convenient shape invented here.

use flint::config::{Config, SearchConfig};
use flint::tools::ToolBox;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A response as DeepSeek actually sent one, trimmed to what this test needs.
fn measured_response() -> serde_json::Value {
    json!({
        "content": [
            { "type": "server_tool_use", "input": { "query": "latest stable Rust release" } },
            { "type": "web_search_tool_result", "content": [
                { "type": "web_search_result", "url": "https://blog.rust-lang.org/1.98.0#1",
                  "title": "Announcing Rust 1.98.0", "page_age": "2026-08-24" },
            ]},
            { "type": "text", "text": "Rust 1.98.0 is the current release." },
        ],
        "usage": { "input_tokens": 16561, "output_tokens": 336,
                   "server_tool_use": { "web_search_requests": 1 } },
    })
}

fn config_for(server: &MockServer) -> Config {
    Config {
        search: Some(SearchConfig {
            base_url: format!("{}/anthropic/v1", server.uri()),
            api_key: "sk-for-the-test".to_string(),
            ..SearchConfig::default()
        }),
        ..Config::default()
    }
}

#[tokio::test]
async fn the_search_tool_asks_deepseek_and_returns_its_answer() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/anthropic/v1/messages"))
        // Two headers that are the whole authentication and API version.
        .and(header("x-api-key", "sk-for-the-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(measured_response()))
        .mount(&server)
        .await;

    let tools = ToolBox::new(&config_for(&server), false, std::env::temp_dir());
    assert!(
        tools.names().iter().any(|n| n == "search"),
        "search is configured and must be offered: {:?}",
        tools.names()
    );

    let out = tools
        .invoke("search", &json!({ "query": "latest stable rust release" }))
        .await
        .expect("the search must run");

    assert!(out.contains("Rust 1.98.0 is the current release."), "{out}");
    assert!(out.contains("https://blog.rust-lang.org/1.98.0"), "{out}");
    assert!(!out.contains("#1"), "the fragment is DeepSeek's, not the page's: {out}");

    // The request that went out is the search request, not a chat one.
    let requests = server.received_requests().await.expect("requests");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("JSON");
    assert_eq!(body["tools"][0]["type"], "web_search_20250305", "{body}");
    assert_eq!(body["stream"], serde_json::Value::Null, "search is not streamed");
}

/// A refusal from the endpoint is reported with the endpoint's own words, because that is
/// what a person can act on: a bad model name, a key without access, a rate limit.
#[tokio::test]
async fn a_refusal_says_what_the_endpoint_said() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/anthropic/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": { "message": "Authentication Fails, Your api key is invalid" }
        })))
        .mount(&server)
        .await;

    let tools = ToolBox::new(&config_for(&server), false, std::env::temp_dir());
    let error = tools
        .invoke("search", &json!({ "query": "anything" }))
        .await
        .expect_err("401 is a failure");
    let message = format!("{error:#}");
    assert!(message.contains("401"), "{message}");
    assert!(message.contains("api key is invalid"), "{message}");
}

/// A response that never triggered a search is the failure that looks like success.
#[tokio::test]
async fn an_answer_without_a_search_is_reported_rather_than_shown_as_empty() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/anthropic/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content": [{ "type": "text", "text": "I cannot browse the web." }],
        })))
        .mount(&server)
        .await;

    let tools = ToolBox::new(&config_for(&server), false, std::env::temp_dir());
    let error = tools
        .invoke("search", &json!({ "query": "anything" }))
        .await
        .expect_err("no result block means no search");
    assert!(format!("{error:#}").contains("did not search"), "{error:#}");
}

/// No configuration, no tool: a schema that can only ever fail costs every request and
/// teaches the model that this tool is broken.
#[tokio::test]
async fn the_tool_is_not_offered_when_nothing_can_search() {
    let config = Config {
        search: None,
        providers: Vec::new(),
        ..Config::default()
    };
    let tools = ToolBox::new(&config, false, std::env::temp_dir());
    assert!(
        !tools.names().iter().any(|n| n == "search"),
        "a tool that cannot work must not be advertised: {:?}",
        tools.names()
    );
}
