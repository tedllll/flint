//! Provider layer: one OpenAI-compatible streaming client.
//!
//! Every mainstream endpoint (DeepSeek, Kimi, GLM, OpenRouter, Together,
//! Ollama, vLLM, llama.cpp) speaks this protocol, so a single implementation
//! covers all of them. Native protocols (Anthropic's `/v1/messages`) are
//! deliberately out of scope for the rescue tool.
//!
//! The two things that actually bite here, and which the tests below pin down:
//!   1. SSE frames split across network chunks — handled by keeping a
//!      carry-over buffer and only consuming complete lines.
//!   2. `tool_calls[].function.arguments` arrives as string *fragments* that
//!      must be concatenated by index before the JSON is valid.
//!
//! `StreamParser` is deliberately a plain, synchronous state machine with no
//! I/O so it can be tested against recorded provider payloads.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::ProviderConfig;
use crate::event::{Event, Message, ToolCall, Usage};

#[derive(Clone)]
pub struct Provider {
    config: ProviderConfig,
    client: reqwest::Client,
    /// Whether this run asked for a JSON object (`--schema`).
    ///
    /// On the client rather than in the body's arguments because it is a property of the *run*, not
    /// of one request: every turn of a schema run is a schema turn, including the repair turns that
    /// follow a bad answer, and threading it through each call would be one more place to forget it.
    json_mode: bool,
    /// How much reasoning this run is asking for, and in which field. See [`Thinking`].
    ///
    /// Here for `json_mode`'s reason: a level holds for every turn of a run, and `/thinking` changes
    /// it while the run is open, so it is state rather than an argument.
    thinking: Thinking,
}

/// A reasoning level, and the JSON field to carry it in.
///
/// Two facts because this area has no standard, and naming both is the alternative to a
/// compatibility table. The *level* is flint's, a small ladder a person can hold in their head; the
/// *field* is the endpoint's, named by [`crate::config::ProviderConfig::thinking_field`], because
/// vendors disagree about it and flint will not keep a census of other people's servers. Pi does keep
/// one -- `reasoning_effort`, `openrouter`, `deepseek`, `together`, `qwen`, `chat-template` -- with
/// its own comment that "Grok models don't like `reasoning_effort`", and the lesson taken from
/// reading it is the opposite of copying it: the value is standard, the field is not, so the person
/// says which field their endpoint wants and flint asks in that one.
///
/// Nothing is sent unless *both* are set. A run at `off`, or a provider with no `thinking_field`, has
/// exactly the request body it had before this existed -- which is what keeps a wrong guess about
/// somebody's endpoint from arriving as a 400 in the middle of a turn.
#[derive(Clone, Debug)]
pub struct Thinking {
    /// `off`, `low`, `medium` or `high`.
    pub level: String,
    /// The field name from the provider's config. Empty means "do not ask".
    pub field: String,
}

impl Thinking {
    /// The levels flint asks in, which is also the order the page's switch offers them in.
    ///
    /// Deliberately shorter than Pi's (`off|minimal|low|medium|high|xhigh|max`): flint cannot check
    /// which rungs a given model has, and offering seven words an endpoint may not know is a menu
    /// that lies. Four cover "none, some, more, most" and every one of them is a word in real use.
    pub const LEVELS: [&'static str; 4] = ["off", "low", "medium", "high"];

    /// Whether `word` is a level this build knows.
    pub fn is_a_level(word: &str) -> bool {
        Self::LEVELS.contains(&word)
    }

    /// The field and the level to put in the request, or `None` when nothing is asked for.
    pub fn asked(&self) -> Option<(&str, &str)> {
        let field = self.field.trim();
        if self.level == "off" || field.is_empty() {
            None
        } else {
            Some((field, self.level.trim()))
        }
    }
}

/// Which proxy to reach this provider through -- an explicit one, or none.
///
/// "None" is the default and it means *direct*, not "whatever the platform thinks".
/// reqwest otherwise picks up the Windows system proxy from the registry, which is a
/// setting the user may not have chosen and cannot see from flint: on a machine where a
/// proxy client is installed but has no server selected, `ProxyEnable=1` points at a
/// closed port and every request dies inside a tunnel nothing owns. The symptom is a
/// working network and an agent that cannot connect -- with no configured proxy to
/// blame, because there is not one.
///
/// So the only proxy is the one written down for this provider. An explicit setting is
/// honoured; silence means connect directly.
pub fn configured_proxy(config: &ProviderConfig) -> Option<String> {
    config
        .proxy
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
}

/// Whether a base URL points at a local model server.
///
/// Used to keep local endpoints off any configured proxy: the local provider is
/// the one that still works when the network is what broke, so it must not
/// depend on the network path being healthy.
pub fn is_local_endpoint(base_url: &str) -> bool {
    let u = base_url.to_ascii_lowercase();
    u.contains("localhost")
        || u.contains("127.0.0.1")
        || u.contains("0.0.0.0")
        || u.contains("[::1]")
}

/// Guarantee the one shape the API insists on: every tool call an assistant message
/// asked for is answered by a tool message right after it.
///
/// The agent repairs its own history, so this should have nothing left to do. It runs
/// anyway because the cost of the API rejecting a request is the whole session being
/// unusable, and the check is cheap; a provider that only sometimes produces valid
/// payloads is not one you can rescue a broken machine with.
fn ensure_tool_calls_are_answered(messages: &[Message]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut index = 0;
    while index < messages.len() {
        let message = messages[index].clone();
        out.push(message.clone());
        let Message::Assistant { tool_calls, .. } = &message else {
            index += 1;
            continue;
        };
        if tool_calls.is_empty() {
            index += 1;
            continue;
        }
        let mut answered: Vec<String> = Vec::new();
        let mut next = index + 1;
        while let Some(Message::Tool { tool_call_id, .. }) = messages.get(next) {
            answered.push(tool_call_id.clone());
            out.push(messages[next].clone());
            next += 1;
        }
        for call in tool_calls {
            if !answered.contains(&call.id) {
                out.push(Message::Tool {
                    tool_call_id: call.id.clone(),
                    content: format!(
                        "interrupted by the user: tool '{}' was requested but never ran.",
                        call.name
                    ),
                });
            }
        }
        index = next;
    }
    out
}

/// How many times one completion is attempted before the turn is reported as failed.
const MAX_ATTEMPTS: u32 = 4;

/// How long to wait before attempt `n` (1-based: the wait after attempt 1 is the first).
///
/// Exponential with a one-second floor and a floor on the growth of the clock, so a
/// provider that is down does not get hammered and a blip costs a fraction of a second.
fn retry_delay(attempt: u32) -> Duration {
    let secs = 1u64 << (attempt.saturating_sub(1)).min(4);
    Duration::from_secs(secs.min(RETRY_CAP_SECS))
}

const RETRY_CAP_SECS: u64 = 16;

/// The backoff schedule, exposed so its shape can be asserted rather than guessed at.
///
/// Not `pub(crate)`: it is only interesting to the test that pins the ladder's bounds.
#[doc(hidden)]
pub fn retry_delay_for_test(attempt: u32) -> Duration {
    retry_delay(attempt)
}

/// First line of a message, for the one-line notice printed between attempts.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

/// Why a provider call failed, and what a caller can do about it.
///
/// A type rather than a sentence, for the same reason `Usage` in `main` is one: the classification
/// has to be made where the response was read, and read back from the error itself afterwards
/// rather than guessed from the message. `retryable` rides along because whether waiting helps is a
/// fact about *this* failure -- and because deciding it from the status code alone is exactly how an
/// exhausted balance gets retried four times. See `classify`.
#[derive(Debug, Clone)]
pub struct ProviderFailure {
    /// A stable name for the cause, for a program: `insufficient_balance`, `rate_limit`, `auth`,
    /// `server`, `bad_request`, `network`, `unknown`.
    pub code: &'static str,
    /// Whether another attempt could plausibly help.
    pub retryable: bool,
    /// What the provider said, and where.
    pub message: String,
}

impl ProviderFailure {
    /// The request did not get out, or the answer stopped arriving.
    ///
    /// Retryable: a dropped socket usually clears. Note that a balance running out *mid-answer*
    /// arrives here too, because a provider that cuts the stream does not say why -- and that is
    /// precisely why the turn's `outcome` matters more than this code in that case.
    fn network(message: impl Into<String>) -> Self {
        Self {
            code: "network",
            retryable: true,
            message: message.into(),
        }
    }

    /// A response the provider answered, classified from the status *and* the body.
    fn from_response(provider: &str, status: reqwest::StatusCode, body: &str) -> Self {
        let (code, retryable) = classify(status, body);
        Self {
            code,
            retryable,
            message: format!(
                "provider '{}' returned HTTP {}: {}",
                provider,
                status,
                crate::util::truncate(body.trim(), 800)
            ),
        }
    }
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderFailure {}

/// What a non-2xx response means, read from the status *and* the body.
///
/// The body has to be read because the status is not the classification, and the case that proves it
/// is money: an OpenAI-shaped endpoint reports an exhausted quota as `429`, which is the same status
/// a rate limit arrives with, and flattening the two means retrying a failure that cannot succeed --
/// four attempts and fifteen seconds of backoff per call, in a batch that will hit it a hundred
/// times. DeepSeek says it with `402`, Anthropic with a `400` and a sentence, and all three are the
/// same fact about the account.
///
/// The fallback keeps the old behaviour (`status_is_transient`) rather than inventing a stricter one:
/// a status nobody has taught this function about still gets the retries it got before, so the only
/// failure that stops being retried is the one that provably cannot succeed.
fn classify(status: reqwest::StatusCode, body: &str) -> (&'static str, bool) {
    let said = body.to_ascii_lowercase();
    let money = said.contains("insufficient_quota")
        || said.contains("insufficient balance")
        || said.contains("credit balance is too low");
    if status == reqwest::StatusCode::PAYMENT_REQUIRED || money {
        return ("insufficient_balance", false);
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return ("auth", false);
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return ("rate_limit", true);
    }
    if status.is_server_error() {
        return ("server", true);
    }
    if status.is_client_error() {
        return ("bad_request", false);
    }
    ("unknown", status_is_transient(status))
}

/// Whether a status is worth another attempt.
///
/// 429 is the provider asking us to slow down and will usually clear; 5xx is the
/// provider being briefly broken. Both are transient by definition, and both are exactly
/// the cases where giving up loses a turn that would have succeeded a second later.
fn status_is_transient(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// The body of one completion request.
///
/// A free function, and the *only* place the request is serialised, because `flint debug
/// prompt-input` has to print exactly what would be sent. A second copy of this in the
/// debug path would be a preview of something that is not going to be sent, and it would
/// be wrong in the way that is hardest to notice: right on the day it was written.
///
/// `json_mode` asks the server for a JSON object instead of prose, and is set from the schema the
/// caller gave (`--schema`): the *shape* is not the server's to enforce, because the only JSON mode
/// the OpenAI-compatible surface agrees on is `json_object` -- DeepSeek rejects `json_schema`
/// outright -- so the shape goes into the prompt and flint checks the answer itself. This flag is
/// what makes the provider refuse to hand back prose at all.
///
/// `ensure_tool_calls_are_answered` is part of the body rather than a caller's job for the
/// same reason -- it changes what the model is shown, so a preview that skipped it would
/// be showing a conversation the provider never receives.
pub fn request_body(
    model: &str,
    messages: &[Message],
    tools: &[(String, String, Value)],
    json_mode: bool,
    thinking: &Thinking,
) -> Value {
    let tools_payload: Vec<Value> = tools
        .iter()
        .map(|(name, description, parameters)| {
            json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                }
            })
        })
        .collect();

    let mut body = json!({
        "model": model,
        "messages": ensure_tool_calls_are_answered(messages),
        "stream": true,
        // Ask for a final usage frame; harmless for servers that ignore it.
        "stream_options": { "include_usage": true },
    });
    if !tools_payload.is_empty() {
        body["tools"] = json!(tools_payload);
    }
    if json_mode {
        body["response_format"] = json!({ "type": "json_object" });
    }
    // The reasoning level, when the person asked for one *and* this provider named the field for it.
    // Last, so the two conditions are in one place and a reader can see that neither alone sends
    // anything: a level with no field is silence, and a field with no level is the default.
    if let Some((field, level)) = thinking.asked() {
        body[field] = json!(level);
    }
    body
}

/// What a provider can be asked about itself, cheaply and without spending anything.
///
/// `usable` is an `Option` because the honest answer is sometimes "cannot tell", and a preflight whose
/// point is to be trusted before a batch must not report a verdict it did not establish. A local
/// engine that does not implement `GET /models` is reachable and may be perfectly usable, and calling
/// that "not usable" would make the command wrong in the direction that costs a caller real work.
#[derive(Debug, Clone)]
pub struct Balance {
    /// `Some(true)` when the provider says it will serve requests, `Some(false)` when it says it will
    /// not, `None` when the check could not decide.
    pub usable: Option<bool>,
    /// What answered the question, so the caller knows what was actually checked: `balance` (the
    /// provider's own balance endpoint), `models` (key and reachability, nothing about money), or
    /// `none` (the endpoint was reached but answered neither).
    pub checked: &'static str,
    /// The currency and the three amounts, when the provider publishes them and only then.
    pub currency: Option<String>,
    pub total: Option<String>,
    pub granted: Option<String>,
    pub topped_up: Option<String>,
}

impl Balance {
    /// A verdict with no figures, which is what every check but DeepSeek's produces.
    fn verdict(usable: Option<bool>, checked: &'static str) -> Self {
        Self {
            usable,
            checked,
            currency: None,
            total: None,
            granted: None,
            topped_up: None,
        }
    }
}

impl Provider {
    /// A client that is valid but points nowhere, for when the real one cannot be built.
    ///
    /// Used so a failure to configure the client does not stop the process from opening
    /// its own session history. Nothing is ever sent through it: the deferred error is
    /// reported before the first request.
    pub fn fallback_config() -> ProviderConfig {
        ProviderConfig {
            name: "unavailable".to_string(),
            base_url: "http://127.0.0.1:1".to_string(),
            api_key: String::new(),
            model: String::new(),
            models: Vec::new(),
            api_key_env: None,
            start: None,
            stop: None,
            start_timeout_secs: 0,
            proxy: None,
            thinking_field: String::new(),
        }
    }

    /// Ask whether this provider can be used, without spending a token.
    ///
    /// Two questions, and which one can be answered depends on the provider. DeepSeek publishes
    /// `GET /user/balance`, whose `is_available` its own documentation defines as "whether the user's
    /// balance is sufficient for API calls" -- the cause that `classify` names when a run fails with
    /// `insufficient_balance`, asked *before* a batch instead of after. Everything else that speaks
    /// this protocol exposes `GET /models`, which proves the key and the route and says nothing about
    /// money; flint asks the money question only where it can be answered, because a wrong balance is
    /// worse than no balance.
    pub async fn balance(&self) -> std::result::Result<Balance, ProviderFailure> {
        let key = self.config.resolved_key();
        if key.trim().is_empty() {
            // The same code `main` gives a run with no key, and the same advice: this is the one
            // failure a preflight is most useful for, because it costs nothing to find out.
            return Err(ProviderFailure {
                code: "no_key",
                retryable: false,
                message: format!(
                    "provider '{}' has no API key. Set `api_key` or `api_key_env` for it.",
                    self.config.name
                ),
            });
        }

        let root = self.config.api_root();
        // The money question is asked first and by *behaviour*, not by provider name: an endpoint that
        // answers `/user/balance` has one, and one that answers 404 does not. Asking DeepSeek's name
        // would have been a guess that misses a DeepSeek-compatible gateway, a proxy in front of it,
        // and every test -- and it would have made "no balance API" and "not DeepSeek" the same fact.
        let (status, body) = self.get(&format!("{root}/user/balance"), &key).await?;
        if status.is_success() {
            // A body without `is_available` is not a balance flint can read, and it is not a pass
            // either: `checked` says the balance endpoint answered, `usable` says flint did not learn
            // the verdict from it.
            let value = serde_json::from_str::<Value>(&body).ok();
            let available = value
                .as_ref()
                .and_then(|v| v.get("is_available"))
                .and_then(|v| v.as_bool());
            let Some(available) = available else {
                return Ok(Balance::verdict(None, "none"));
            };
            let mut balance = Balance::verdict(Some(available), "balance");
            // The first entry, which is the one DeepSeek documents: a multi-currency account is not
            // something flint sums up, because adding CNY to USD would be a number nobody can act on.
            if let Some(info) = value
                .as_ref()
                .and_then(|v| v.get("balance_infos"))
                .and_then(|v| v.as_array())
                .and_then(|all| all.first())
            {
                let text = |key: &str| {
                    info.get(key)
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                };
                balance.currency = text("currency");
                balance.total = text("total_balance");
                balance.granted = text("granted_balance");
                balance.topped_up = text("topped_up_balance");
            }
            return Ok(balance);
        }
        if status != reqwest::StatusCode::NOT_FOUND && status != reqwest::StatusCode::METHOD_NOT_ALLOWED
        {
            // A 402 here is the account being empty, classified by the same function that classifies a
            // failed turn -- which is the point: one table, one vocabulary.
            return Err(ProviderFailure::from_response(&self.config.name, status, &body));
        }
        // No balance endpoint. `GET /models` is the other question this protocol answers everywhere:
        // it proves the key and the route, and says nothing about money.
        let (status, _) = self.get(&format!("{root}/models"), &key).await?;
        if status.is_success() {
            return Ok(Balance::verdict(Some(true), "models"));
        }
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::METHOD_NOT_ALLOWED
        {
            // Reached, answered, and answered nothing flint can use. Not a failure of the provider and
            // not a pass: a local engine that serves only `/chat/completions` lands here, and saying
            // "usable" would be a claim nothing checked.
            return Ok(Balance::verdict(None, "none"));
        }
        Err(ProviderFailure::from_response(
            &self.config.name,
            status,
            "",
        ))
    }

    /// One GET, with the same proxy and timeout treatment as a completion.
    ///
    /// Not retried: a preflight is a question asked *before* spending anything, and a caller that
    /// wants to wait has the retry ladder of the real call. A transport failure is still classified,
    /// so `75` reaches the shell and a batch knows to try later rather than to give up.
    async fn get(
        &self,
        url: &str,
        key: &str,
    ) -> std::result::Result<(reqwest::StatusCode, String), ProviderFailure> {
        let resp = match tokio::time::timeout(
            Duration::from_secs(30),
            self.client.get(url).bearer_auth(key).send(),
        )
        .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                return Err(ProviderFailure::network(format!(
                    "no network -- the check of {url} did not get out.{}{e:#}",
                    self.proxy_note()
                        .map(|n| format!("\n{n}\n  "))
                        .unwrap_or_default()
                )))
            }
            Err(_) => {
                return Err(ProviderFailure::network(format!(
                    "no response from {url} after 30s -- the check is not worth waiting longer for.{}",
                    self.proxy_note().map(|n| format!("\n{n}")).unwrap_or_default()
                )))
            }
        };
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Ok((status, body))
    }

    /// One line naming the proxy in force, for an error message.
    ///
    /// Only ever present when one was configured, so its absence is a real signal: the
    /// request went out directly. Silence is the useful default here -- there is nothing
    /// to say about a proxy when there is not one.
    fn proxy_note(&self) -> Option<String> {
        let proxy = configured_proxy(&self.config)?;
        let host_port = proxy
            .rsplit("://")
            .next()
            .unwrap_or(&proxy)
            .trim_end_matches('/')
            .to_string();
        let listening = host_port
            .parse()
            .ok()
            .and_then(|addr| {
                std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()
            })
            .is_some();
        Some(format!(
            "  configured proxy: {proxy}{}",
            if listening {
                ""
            } else {
                "  (nothing is listening there)"
            }
        ))
    }

    pub fn new(config: ProviderConfig) -> Result<Self> {
        let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(30));
        // No total timeout here: a long generation is not a failure. The wait for the
        // *response* is bounded at the call site instead.

        // Direct by default. `no_proxy()` is not decoration: without it reqwest applies
        // the platform's proxy setting, which on Windows comes from the registry and is
        // invisible from here. A configured proxy is then added on top, deliberately and
        // explicitly, so what is used is exactly what is written down.
        builder = builder.no_proxy();
        if let Some(proxy) = configured_proxy(&config) {
            let url = if proxy.contains("://") {
                proxy.clone()
            } else {
                format!("http://{proxy}")
            };
            builder = builder.proxy(
                reqwest::Proxy::all(&url)
                    .with_context(|| format!("proxy '{proxy}' is not a usable URL"))?,
            );
        }

        let client = builder.build().context("cannot build HTTP client")?;
        // Read before `config` is moved into the struct. The *level* starts at `off` and is set by the
        // run that owns this provider (`Agent::hold_to_thinking`), because it is a choice about the
        // conversation rather than a fact about the endpoint -- which is exactly what the split
        // between this and `thinking_field` is for.
        let thinking = Thinking {
            level: "off".to_string(),
            field: config.thinking_field.clone(),
        };
        Ok(Provider {
            config,
            client,
            json_mode: false,
            thinking,
        })
    }

    /// Ask for a JSON object instead of prose, or stop asking.
    ///
    /// Set once per change of shape, and not per request: a repair turn after a bad answer is still
    /// a schema turn, so the flag belongs to the run rather than to one call.
    pub fn expect_json(&mut self, on: bool) {
        self.json_mode = on;
    }

    /// Set the reasoning level for this run, keeping the provider's field.
    ///
    /// `/thinking` is the only caller, and it can only set a level: the field is the endpoint's and
    /// belongs in the file a person edits, not in a command.
    pub fn set_thinking(&mut self, level: &str) {
        self.thinking.level = level.trim().to_string();
    }

    /// The reasoning level in force.
    pub fn thinking(&self) -> &str {
        &self.thinking.level
    }

    /// The field this provider carries a level in, empty when it carries none.
    pub fn thinking_field(&self) -> &str {
        self.thinking.field.trim()
    }

    /// The level and the field together, for the one caller that builds a body without sending it
    /// ([`crate::agent::Agent::request_preview`]): the preview has to carry both exactly as the
    /// request will, or it is a preview of a different request.
    pub fn thinking_spec(&self) -> &Thinking {
        &self.thinking
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Stream one completion, invoking `on_event` for every incremental update.
    pub async fn stream_chat(
        &self,
        messages: &[Message],
        tools: &[(String, String, Value)],
        mut on_event: impl FnMut(Event),
    ) -> Result<()> {
        // Built by the same function `flint debug prompt-input` prints, so the preview
        // cannot drift from the request that is actually sent.
        let body = request_body(
            &self.config.model,
            messages,
            tools,
            self.json_mode,
            &self.thinking,
        );

        let key = self.config.resolved_key();

        // One HTTP request is not one attempt. A stream that dies halfway through is the
        // normal failure of a long turn on a flaky link, and reporting it as a failed turn
        // throws away everything the model had already produced -- including, often, the
        // part that says what it was doing.
        //
        // **The retry is only free while nothing has been drawn.** Events go out as they
        // arrive, because a terminal, a browser and a `--json` reader all want the answer
        // while it is being written -- and that makes each one of them irreversible: a
        // terminal has already scrolled the line, a pipe has already emitted it, the browser
        // has already rendered it. Retrying after that prints the second attempt after the
        // first, and the answer appears twice.
        //
        // So the ladder stops at the first drawn character. Everything before it -- the
        // connection that never opened, the 503, the stream that died during the *reasoning*
        // -- is still retried and still leaves no trace, which is where the retries were
        // most of their value anyway.
        let mut attempt = 0u32;
        let mut last_error: Option<anyhow::Error>;

        loop {
            attempt += 1;
            let mut req = self.client.post(self.config.endpoint()).json(&body);
            if !key.is_empty() {
                req = req.bearer_auth(&key);
            }

            // Whether this attempt put text in front of someone, and therefore whether a
            // retry is still free.
            let mut drawn = false;
            let outcome = self
                .attempt_stream(req, &mut |event| {
                    if matches!(event, Event::Text(_)) {
                        drawn = true;
                    }
                    on_event(event);
                })
                .await;

            match outcome {
                Ok(()) => return Ok(()),
                Err(failure) => {
                    // The classification is carried, not flattened into a string: `main` reads it to
                    // choose the exit code and to name the cause on the stream, and a caller that has
                    // to match text to learn that the account is empty is the fault this type exists
                    // to remove. `retryable` is asked of the failure rather than re-derived here,
                    // because only the code that read the response can tell a rate limit from an
                    // empty account -- they arrive as the same status.
                    let retryable = failure.retryable;
                    last_error = Some(anyhow::Error::new(failure));

                    // Only *text* stops a retry. Reasoning goes to the status line, which is
                    // a word and a clock that get repainted anyway -- and on a reasoning
                    // model it arrives within a second of the request, so treating it as
                    // drawn would disable the ladder for exactly the models that spend the
                    // longest producing an answer.
                    if drawn {
                        let why = last_error
                            .take()
                            .unwrap_or_else(|| anyhow::anyhow!("the provider gave no response"));
                        // Same reason as below: the sentence is context, not a replacement for the
                        // classification -- this can be a balance that ran out mid-answer, and `main`
                        // still has to see which kind of failure it was.
                        return Err(why.context(
                            "the answer had already begun to arrive, so it was not retried -- a \
                             second attempt would have been written after the first. What arrived \
                             is above; ask again for the rest.",
                        ));
                    }

                    if !retryable || attempt >= MAX_ATTEMPTS {
                        break;
                    }
                    let wait = retry_delay(attempt);
                    let why = last_error
                        .as_ref()
                        .map(|e| format!("{e:#}"))
                        .unwrap_or_default();
                    // Through the notice sink rather than `eprintln!`: this fires from inside the
                    // request loop, so it is always during a turn, and with the strip active a stray
                    // write to stderr lands inside the answer being drawn. The sink falls back to
                    // stderr for a run with no UI (`--json`, a test), where that is the right place.
                    crate::tools::notice(&format!(
                        "{} -- retrying in {}s (attempt {}/{})",
                        first_line(&why),
                        wait.as_secs(),
                        attempt + 1,
                        MAX_ATTEMPTS
                    ));
                    tokio::time::sleep(wait).await;
                }
            }
        }

        let err = last_error.unwrap_or_else(|| anyhow::anyhow!("the provider gave no response"));
        // `.context` rather than formatting the note into the message: the note is for a person
        // reading a terminal, and the classification underneath is for `main`, which has to still be
        // able to find it after the retries gave up.
        Err(if attempt > 1 {
            err.context(format!(
                "gave up after {attempt} attempts -- the network or the provider stayed unreachable"
            ))
        } else {
            err
        })
    }

    /// One attempt at the request, buffering its events instead of emitting them.
    ///
    /// Events go into `pending` so a failed attempt leaves no trace: the caller only
    /// hands them to `on_event` once the whole response has arrived.
    async fn attempt_stream(
        &self,
        req: reqwest::RequestBuilder,
        on_event: &mut dyn FnMut(Event),
    ) -> std::result::Result<(), ProviderFailure> {
        // A dead network has to be *named*, and so does the proxy in front of it. The
        // reader's question is whether to wait, fix the cable, start the proxy, or clear
        // the proxy setting -- and reqwest's bare "error sending request" answers none
        // of them.
        let resp = match tokio::time::timeout(Duration::from_secs(180), req.send()).await {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                let endpoint = self.config.endpoint();
                return Err(ProviderFailure::network(format!(
                    "no network -- the request to {endpoint} did not get out.{}{e:#}",
                    self.proxy_note()
                        .map(|n| format!("\n{n}\n  "))
                        .unwrap_or_default()
                )));
            }
            Err(_) => {
                return Err(ProviderFailure::network(format!(
                    "no response from {} after 180s -- the connection was established but \
                     the server never answered. Usually the network dropped, or a proxy is \
                     swallowing the request.{}",
                    self.config.endpoint(),
                    self.proxy_note().map(|n| format!("\n{n}")).unwrap_or_default()
                )));
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderFailure::from_response(&self.config.name, status, &text));
        }

        let mut stream = resp.bytes_stream();
        let mut parser = StreamParser::default();
        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| {
                ProviderFailure::network(format!(
                    "the connection to {} dropped while the answer was streaming: {e}",
                    self.config.endpoint()
                ))
            })?;
            buffer.push_str(&String::from_utf8_lossy(&bytes));

            // Only consume whole lines; a partial frame stays in the buffer.
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer.drain(..=pos);
                // Handed over as they are parsed, not collected: this is what makes the
                // answer appear while it is being written.
                for event in parser.feed_line(&line) {
                    on_event(event);
                }
                if parser.done {
                    break;
                }
            }

            if parser.done {
                break;
            }
        }

        // A stream that ended without the provider's completion signal is not a short
        // answer, it is a broken one, and this is the shape a dropped connection usually
        // takes: an interrupted stream, a proxy that gave up, a server that died -- they all
        // end the body, and a body ending looks exactly like a response being over. Without
        // this the half answer that arrived becomes *the* answer, and the turn reads as a
        // success. Found while making the same code stream: `parser.done` was only ever used
        // to leave the read loop early, never asked afterwards.
        if !parser.done {
            return Err(ProviderFailure::network(format!(
                "the response from {} ended before it was finished -- no completion signal \
                 arrived, so what came back is only part of an answer",
                self.config.endpoint()
            )));
        }

        // Surface the fully assembled tool calls so the agent can execute them. `finish`
        // runs here rather than at the caller because the calls are only complete once the
        // whole response has arrived, and nothing before that point knows what they are.
        for event in parser.finish() {
            on_event(event);
        }
        Ok(())
    }
}

/// Incremental parser for an OpenAI-compatible SSE stream.
///
/// Feed it one complete line at a time. It never blocks, never allocates a
/// runtime, and holds exactly the state needed to reassemble tool calls.
#[derive(Default)]
pub struct StreamParser {
    /// Tool calls under construction, keyed by the provider's `index`.
    calls: BTreeMap<u64, ToolCall>,
    /// Set when the `[DONE]` sentinel is seen.
    pub done: bool,
    /// Set when a `finish_reason` was observed.
    pub finish_reason: Option<String>,
}

impl StreamParser {
    /// Parse one line, returning any events it produced.
    ///
    /// Non-data lines, keep-alives and unparseable payloads are ignored rather
    /// than treated as errors: providers insert all three.
    pub fn feed_line(&mut self, line: &str) -> Vec<Event> {
        let mut events = Vec::new();

        let Some(data) = line.strip_prefix("data:") else {
            return events;
        };
        let data = data.trim();
        if data.is_empty() {
            return events;
        }
        if data == "[DONE]" {
            self.done = true;
            return events;
        }

        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(e) => {
                // A payload that *looks* like JSON but does not parse means the
                // provider changed something. Staying silent here is how you
                // get an agent that mysteriously does nothing, so say so.
                if data.starts_with('{') {
                    events.push(Event::Warning(format!(
                        "could not parse a stream frame from the provider: {e}. Raw: {}",
                        crate::util::truncate(data, 200)
                    )));
                }
                return events;
            }
        };

        // A usage-only frame is the final frame on OpenAI and DeepSeek.
        if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
            events.push(Event::Usage(usage_from(usage)));
        }

        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            return events;
        };

        for choice in choices {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                if !reason.is_empty() {
                    self.finish_reason = Some(reason.to_string());
                }
            }

            let Some(delta) = choice.get("delta") else {
                continue;
            };

            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events.push(Event::Text(text.to_string()));
                }
            }
            // The reasoning channel, under either of the two names that exist for it.
            //
            // `reasoning_content` is DeepSeek's and the one this started with.
            // `reasoning` is what mlx_lm.server sends -- measured against a local model,
            // where flint showed no reasoning at all because it only knew the first name.
            // Both are read rather than one being chosen: a local model and a hosted one
            // are the same conversation to whoever is reading the status line.
            let reasoning = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .and_then(Value::as_str);
            if let Some(reasoning) = reasoning {
                if !reasoning.is_empty() {
                    events.push(Event::Reasoning(reasoning.to_string()));
                }
            }

            if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tcs {
                    let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let entry = self.calls.entry(index).or_default();

                    if let Some(id) = tc.get("id").and_then(Value::as_str) {
                        if !id.is_empty() {
                            entry.id = id.to_string();
                        }
                    }

                    if let Some(func) = tc.get("function") {
                        // The function name is the last thing we need before the
                        // call is identifiable; the `ToolStart` event is emitted
                        // from `finish()` so it can carry the provider's real id.
                        if let Some(name) = func.get("name").and_then(Value::as_str) {
                            if !name.is_empty() && entry.name.is_empty() {
                                entry.name = name.to_string();
                            }
                        }
                        // Arguments arrive in fragments: append, never replace.
                        if let Some(args) = func.get("arguments").and_then(Value::as_str) {
                            entry.arguments.push_str(args);
                        }
                    }
                }
            }
        }

        events
    }

    /// Completed tool calls. Call once the stream ends.
    ///
    /// Emits `ToolStart` and `ToolArgs` together, using the *same* id, so the
    /// consumer can always correlate them. The id is resolved here rather than
    /// when the function name arrives because the provider may not have sent
    /// its real id yet at that point — synthesising one early would produce two
    /// different ids for one call, and the arguments would be lost.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for (index, call) in std::mem::take(&mut self.calls) {
            if call.name.is_empty() {
                continue;
            }
            let id = if call.id.is_empty() {
                id_for(index)
            } else {
                call.id.clone()
            };
            let arguments = if call.arguments.trim().is_empty() {
                "{}".to_string()
            } else {
                call.arguments.clone()
            };
            events.push(Event::ToolStart {
                id: id.clone(),
                name: call.name.clone(),
            });
            events.push(Event::ToolArgs {
                id,
                args: arguments,
            });
        }
        events
    }
}

/// Providers are inconsistent about ids for streamed tool calls; synthesise a
/// stable one when absent so tool results can be correlated.
pub fn id_for(index: u64) -> String {
    format!("call_{index}")
}

/// A `usage` object read into flint's own accounting, cache split included.
///
/// The counts fall back to `0` because a usage frame without them is a frame that reported nothing
/// usable, and a turn showing no tokens is what the line already meant. The cache split is the one
/// field that must *not* fall back: DeepSeek reports `prompt_cache_hit_tokens`, OpenAI reports
/// `prompt_tokens_details.cached_tokens`, and an endpoint that reports neither is saying nothing
/// about caching -- `None` keeps that distinct from a reported miss, which is the difference between
/// "flint did not ask for this" and "the prefix flint builds keeps changing".
fn usage_from(usage: &Value) -> Usage {
    Usage {
        prompt_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        completion_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_hit_tokens: usage
            .get("prompt_cache_hit_tokens")
            .and_then(Value::as_u64)
            .or_else(|| {
                usage
                    .get("prompt_tokens_details")
                    .and_then(|details| details.get("cached_tokens"))
                    .and_then(Value::as_u64)
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(events: &[Event]) -> String {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The status is not the classification, and this is the case that proves it.
    ///
    /// Three providers say "there is no money" three ways, and two of them use a status that means
    /// something else as well. Reading only the status is how an exhausted quota got retried four
    /// times with a fifteen-second ladder, in a batch where every call pays it again.
    #[test]
    fn the_body_says_what_the_status_cannot() {
        use reqwest::StatusCode as S;

        // The same status, two different answers: this pair is the whole reason `classify` exists.
        assert_eq!(
            classify(S::TOO_MANY_REQUESTS, r#"{"error":{"code":"insufficient_quota"}}"#),
            ("insufficient_balance", false),
            "an exhausted quota is not a rate limit"
        );
        assert_eq!(
            classify(S::TOO_MANY_REQUESTS, r#"{"error":{"code":"rate_limit_exceeded"}}"#),
            ("rate_limit", true)
        );
        // DeepSeek's own status, and the body a person would read.
        assert_eq!(
            classify(S::PAYMENT_REQUIRED, r#"{"error":{"message":"Insufficient Balance"}}"#),
            ("insufficient_balance", false)
        );
        // Anthropic: a 400 and a sentence, which is why the body has to be searched.
        assert_eq!(
            classify(S::BAD_REQUEST, "Your credit balance is too low to access the API"),
            ("insufficient_balance", false)
        );
        // The ones that were already right, kept right: a key problem is not worth retrying, and a
        // broken server is.
        assert_eq!(classify(S::UNAUTHORIZED, "{}"), ("auth", false));
        assert_eq!(classify(S::FORBIDDEN, "{}"), ("auth", false));
        assert_eq!(classify(S::SERVICE_UNAVAILABLE, "{}"), ("server", true));
        assert_eq!(classify(S::BAD_REQUEST, "{}"), ("bad_request", false));
        assert_eq!(classify(S::NOT_FOUND, "{}"), ("bad_request", false));
        assert_eq!(
            classify(S::from_u16(418).expect("a teapot"), "{}"),
            ("bad_request", false)
        );
        // A status nobody taught this about -- a redirect where a stream was expected, say -- is
        // retried exactly as it was before: the only failure that stops being retried is the one that
        // provably cannot succeed.
        assert_eq!(
            classify(S::MOVED_PERMANENTLY, "{}"),
            ("unknown", false)
        );
        assert_eq!(
            classify(S::INTERNAL_SERVER_ERROR, "{}"),
            ("server", true)
        );
    }

    /// The exact wire shape the API requires for an assistant turn that calls a
    /// tool. Getting this wrong is fatal but quiet: the first request succeeds,
    /// then every follow-up is rejected with a 400 because the echoed-back
    /// tool_calls are malformed.
    #[test]
    fn assistant_tool_calls_use_the_openai_wire_shape() {
        let msg = Message::Assistant {
            content: None,
            reasoning: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
            }],
        };
        let v = serde_json::to_value(&msg).expect("must serialise");
        let tc = &v["tool_calls"][0];

        assert_eq!(tc["id"], "call_1");
        assert_eq!(
            tc["type"], "function",
            "the `type` discriminator is required, got {tc}"
        );
        assert_eq!(tc["function"]["name"], "bash");
        assert_eq!(tc["function"]["arguments"], r#"{"command":"ls"}"#);
        assert!(
            tc.get("name").is_none(),
            "name must not be emitted at the top level, got {tc}"
        );

        // A tool result must carry the id it answers.
        let tool_msg = Message::Tool {
            tool_call_id: "call_1".to_string(),
            content: "ok".to_string(),
        };
        let tv = serde_json::to_value(&tool_msg).expect("must serialise");
        assert_eq!(tv["role"], "tool");
        assert_eq!(tv["tool_call_id"], "call_1");
    }

    #[test]
    fn parses_plain_text_deltas() {
        let mut p = StreamParser::default();
        let mut all = Vec::new();
        for line in [
            r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#,
            "data: [DONE]",
        ] {
            all.extend(p.feed_line(line));
        }
        assert_eq!(texts(&all), "Hello");
        assert!(p.done);
    }

    #[test]
    fn ignores_comments_keepalives_and_garbage() {
        let mut p = StreamParser::default();
        for line in [
            ": keep-alive",
            "",
            "event: ping",
            "data: not-json",
            r#"data: {"choices":[]}"#,
        ] {
            assert!(
                p.feed_line(line).is_empty(),
                "line {line:?} should be inert"
            );
        }
        assert!(!p.done);
    }

    #[test]
    fn reassembles_tool_call_arguments_from_fragments() {
        let mut p = StreamParser::default();
        // This is the shape DeepSeek and OpenAI actually emit: the function
        // name arrives first, then the arguments in several pieces.
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"bash","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"comm"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"and\":\"ls -la\"}"}}]}}]}"#,
            "data: [DONE]",
        ] {
            p.feed_line(line);
        }

        let finished = p.finish();
        let args = finished
            .iter()
            .find_map(|e| match e {
                Event::ToolArgs { args, .. } => Some(args.clone()),
                _ => None,
            })
            .expect("expected assembled tool args");
        assert_eq!(args, r#"{"command":"ls -la"}"#);
        // And it must be valid JSON by the time the agent sees it.
        let parsed: Value = serde_json::from_str(&args).unwrap();
        assert_eq!(parsed["command"], "ls -la");
    }

    /// Regression test for a bug that silently broke every tool call.
    ///
    /// `ToolStart` used to be emitted when the function name arrived, using an
    /// id synthesised from the stream index. `ToolArgs` was emitted at the end
    /// using the provider's real id. Those two ids differed, so a consumer
    /// correlating on id lost the arguments and invoked the tool with `{}`.
    #[test]
    fn tool_start_and_tool_args_share_the_same_id() {
        let mut p = StreamParser::default();
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"bash","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"command\":\"ls\"}"}}]}}]}"#,
        ] {
            p.feed_line(line);
        }

        let mut started_id = None;
        let mut args_id = None;
        let mut name = None;
        for event in p.finish() {
            match event {
                Event::ToolStart { id, name: n } => {
                    started_id = Some(id);
                    name = Some(n);
                }
                Event::ToolArgs { id, .. } => args_id = Some(id),
                _ => {}
            }
        }

        assert_eq!(
            started_id, args_id,
            "ToolStart and ToolArgs must carry the same id or the call is lost"
        );
        assert_eq!(
            started_id.as_deref(),
            Some("call_abc"),
            "the provider's real id must win"
        );
        assert_eq!(name.as_deref(), Some("bash"));
    }

    /// The id synthesised on `ToolStart` must also match `finish()` when the
    /// provider never sends an id at all.
    #[test]
    fn synthesised_ids_are_consistent_between_start_and_args() {
        let mut p = StreamParser::default();
        p.feed_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":3,"function":{"name":"list","arguments":"{\"path\":\".\"}"}}]}}]}"#,
        );
        let events = p.finish();
        let ids: Vec<String> = events
            .iter()
            .map(|e| match e {
                Event::ToolStart { id, .. } | Event::ToolArgs { id, .. } => id.clone(),
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec!["call_3", "call_3"]);
    }

    #[test]
    fn keeps_parallel_tool_calls_separate() {
        let mut p = StreamParser::default();
        let mut all = Vec::new();
        for line in [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read","arguments":"{\"pa"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"list","arguments":"{\"pa"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"x\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"th\":\".\"}"}}]}}]}"#,
        ] {
            all.extend(p.feed_line(line));
        }
        let finished = p.finish();
        let mut pairs: Vec<(String, String)> = finished
            .iter()
            .filter_map(|e| match e {
                Event::ToolArgs { id, args } => Some((id.clone(), args.clone())),
                _ => None,
            })
            .collect();
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("a".to_string(), r#"{"path":"x"}"#.to_string()),
                ("b".to_string(), r#"{"path":"."}"#.to_string()),
            ]
        );
    }

    /// The reasoning channel has two names in the wild, and a local model uses the other one.
    ///
    /// Found by running flint against `mlx_lm.server`: the reasoning arrived, the parser did
    /// not know the field, and the status line stayed silent for a model that was visibly
    /// thinking.
    #[test]
    fn reasoning_is_read_under_either_name() {
        for field in ["reasoning_content", "reasoning"] {
            let mut p = StreamParser::default();
            let line = format!(r#"data: {{"choices":[{{"delta":{{"{field}":"thinking..."}}}}]}}"#);
            let events = p.feed_line(&line);
            let seen: Vec<&str> = events
                .iter()
                .filter_map(|e| match e {
                    Event::Reasoning(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(seen, vec!["thinking..."], "the {field} field must be read");
        }
    }

    #[test]
    fn extracts_usage_and_reasoning() {
        let mut p = StreamParser::default();
        let mut all = Vec::new();
        all.extend(
            p.feed_line(r#"data: {"choices":[{"delta":{"reasoning_content":"thinking..."}}]}"#),
        );
        all.extend(p.feed_line(
            r#"data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7}}"#,
        ));

        assert!(matches!(all[0], Event::Reasoning(_)));
        match all[1] {
            Event::Usage(u) => {
                assert_eq!(u.prompt_tokens, 100);
                assert_eq!(u.completion_tokens, 7);
                assert_eq!(u.total(), 107);
            }
            ref other => panic!("expected Usage, got {other:?}"),
        }
    }

    /// Both cache shapes the endpoints in the wild use, into one field, and neither invents a zero.
    ///
    /// DeepSeek reports `prompt_cache_hit_tokens`; OpenAI reports `prompt_tokens_details.cached_tokens`.
    /// Reading only one would make the rate silently absent on the other endpoint -- which is the
    /// exact failure the number exists to expose, so it is the one thing this parse must not do. The
    /// last case is the important one: a usage that mentions caching nowhere leaves the field `None`,
    /// because a provider that reports no split and a provider that reports a miss are different.
    #[test]
    fn reads_both_shapes_of_the_cache_split() {
        let usage_of = |line: &str| {
            let mut p = StreamParser::default();
            p.feed_line(line)
                .into_iter()
                .find_map(|e| match e {
                    Event::Usage(u) => Some(u),
                    _ => None,
                })
                .expect("the frame carries usage")
        };

        let deepseek = usage_of(
            r#"data: {"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":7,"prompt_cache_hit_tokens":871,"prompt_cache_miss_tokens":129}}"#,
        );
        assert_eq!(deepseek.cache_hit_tokens, Some(871));
        assert_eq!(deepseek.cache_rate(), Some(87));

        let openai = usage_of(
            r#"data: {"choices":[],"usage":{"prompt_tokens":512,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":512}}}"#,
        );
        assert_eq!(openai.cache_hit_tokens, Some(512));

        // A details object without the field is still "nothing said", not a zero.
        let silent = usage_of(
            r#"data: {"choices":[],"usage":{"prompt_tokens":512,"completion_tokens":3,"prompt_tokens_details":{"audio_tokens":0}}}"#,
        );
        assert_eq!(silent.cache_hit_tokens, None);
        assert_eq!(silent.cache_rate(), None);
    }

    #[test]
    fn records_finish_reason() {
        let mut p = StreamParser::default();
        p.feed_line(r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#);
        assert_eq!(p.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn synthesises_id_when_provider_omits_one() {
        let mut p = StreamParser::default();
        p.feed_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":2,"function":{"name":"list","arguments":"{}"}}]}}]}"#,
        );
        let id = p
            .finish()
            .into_iter()
            .find_map(|e| match e {
                Event::ToolArgs { id, .. } => Some(id),
                _ => None,
            })
            .expect("expected ToolArgs");
        assert_eq!(id, "call_2");
    }

    #[test]
    fn empty_arguments_become_empty_object() {
        let mut p = StreamParser::default();
        p.feed_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"list"}}]}}]}"#,
        );
        let args = p
            .finish()
            .into_iter()
            .find_map(|e| match e {
                Event::ToolArgs { args, .. } => Some(args),
                _ => None,
            })
            .expect("expected ToolArgs");
        assert_eq!(args, "{}");
    }
}
