//! NDJSON: one JSON object per line, for a program reading a run.
//!
//! `flint -p "..." --json` writes the events of a turn as lines on stdout, so a script can
//! follow a run without parsing a terminal. One object per line is the whole format: it can
//! be read with `while read line`, it can be appended to, and a run that dies halfway still
//! leaves every line before it intact.
//!
//! Three rules hold this together, and each of them is a test:
//!
//! * **One line means one line.** A tool result containing newlines, quotes and tabs is
//!   still a single line, because the text is JSON-escaped rather than printed. A reader
//!   that splits on `\n` must never see half an object.
//! * **Every line says what it is.** Every object has a `type` field from a small, closed
//!   vocabulary: `session.started`, `turn.started`, `message.delta`, `reasoning.delta`,
//!   `message.completed`, `tool.started`, `tool.args`, `tool.completed`, `usage`,
//!   `warning`, `status`, `turn.completed`, `error`.
//! * **The same event always writes the same bytes.** Keys come out in a stable order, so
//!   two runs of the same conversation can be compared with `diff` instead of read.
//! * **A stream is a view, not the record.** The session file is still written exactly as
//!   in any other run, so a `--json` run can be listed, resumed and read afterwards like
//!   anything else. Nothing here is the only copy of anything.
//!
//! Deltas are passed through rather than accumulated: a consumer that wants the finished
//! answer waits for `message.completed`, and one that wants to show text as it arrives
//! reads `message.delta`. Both are in the stream, so neither has to guess.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use crate::event::{Event, Usage};

/// Turns the events of one turn into lines.
///
/// Stateful for exactly two reasons: the answer so far (for `message.completed`, since the
/// model emits it as fragments) and the tool name behind each call id (a result that says
/// only `call_2` and `ok` is not worth reading).
#[derive(Default)]
pub struct Sink {
    answer: String,
    names: HashMap<String, String>,
}

impl Sink {
    pub fn new() -> Self {
        Self::default()
    }

    /// What the turn in flight has said so far, without taking it.
    ///
    /// The accumulator `message.completed` drains at the end of a turn is also the only record
    /// of an answer *during* one, and the browser needs it for exactly that window: `/session`
    /// serves the session file, and the file gets the assistant message when the turn ends. A
    /// page that loads, reconnects or is told `reset` in the middle of a turn would otherwise
    /// have no way to learn what has already been said -- measured by reloading a page during a
    /// turn and watching the answer start from the middle.
    pub fn answer_so_far(&self) -> &str {
        &self.answer
    }

    /// Drop the answer in flight, because it is not in flight any more.
    ///
    /// `Done` takes it; this is for the turn that never reaches `Done`, where an interrupt dropped
    /// the future and somebody else committed the text to the conversation. Left here it is not
    /// harmless: `answer_so_far` is what a reader arriving mid-turn is handed as the answer so far,
    /// so a stopped turn's half would be read as one still being written -- and it would be joined
    /// to the next turn's answer, since the accumulator is only ever emptied by `Done`.
    pub fn forget_answer(&mut self) {
        self.answer.clear();
    }

    /// The line for an event, or `None` for the ones that are already fully described by
    /// another event.
    pub fn line(&mut self, event: &Event) -> Option<String> {
        Some(match event {
            Event::Text(text) => {
                self.answer.push_str(text);
                frame("message.delta", json!({ "text": text }))
            }
            Event::Reasoning(text) => frame("reasoning.delta", json!({ "text": text })),
            // Deliberately nothing. A peer's message arrives between turns, and the stream exists only
            // for a one-shot `-p` run -- `--json` refuses a prompt-less run on purpose, because the
            // machine-readable stream is for a caller and a caller has a prompt to give. So there is no
            // `peer.message` frame to promise, and inventing one here would be a frame that never
            // appears. The places a peer's words do reach are the transcript, the page's live feed, and
            // the session file: the three a person looks at.
            Event::Peer { .. } => return None,
            Event::ToolStart { id, name } => {
                self.names.insert(id.clone(), name.clone());
                frame("tool.started", json!({ "id": id, "name": name }))
            }
            Event::ToolArgs { id, args } => frame(
                "tool.args",
                json!({ "id": id, "name": self.names.get(id), "arguments": args }),
            ),
            Event::ToolResult { id, output, ok } => {
                let name = self.names.remove(id);
                frame(
                    "tool.completed",
                    json!({ "id": id, "name": name, "ok": ok, "output": output }),
                )
            }
            Event::Usage(usage) => {
                // `cache_hit_tokens` is inserted rather than always present: a caller computing a hit
                // rate can only do so from a number the endpoint actually reported, and a `0` here
                // would turn "this endpoint does not report caching" into "nothing was cached".
                let mut body = json!({
                    "prompt_tokens": usage.prompt_tokens,
                    "completion_tokens": usage.completion_tokens,
                    "total_tokens": usage.total(),
                });
                if let (Some(hit), Some(object)) = (usage.cache_hit_tokens, body.as_object_mut()) {
                    object.insert("cache_hit_tokens".to_string(), json!(hit));
                }
                frame("usage", body)
            }
            Event::Warning(message) => frame("warning", json!({ "message": message })),
            Event::Status { text, restarted } => status(text, *restarted),
            // The answer is taken, not cloned: a second `Done` in one turn would otherwise
            // repeat the whole message as if it had just been said.
            Event::Done => frame(
                "message.completed",
                json!({ "text": std::mem::take(&mut self.answer) }),
            ),
        })
    }
}

/// The first line of a run: which session it writes to, where it runs, which model.
pub fn session_started(session: Option<&Path>, cwd: &Path, model: &str) -> String {
    frame(
        "session.started",
        json!({
            "session": session.map(|path| path.to_string_lossy().to_string()),
            "cwd": cwd.to_string_lossy(),
            "model": model,
        }),
    )
}

pub fn turn_started(prompt: &str) -> String {
    frame("turn.started", json!({ "prompt": prompt }))
}

/// The same line for a prompt that had files inlined into it: the words as typed, and what went in.
///
/// `prompt` stays the caller's own text on purpose. The expanded prompt is what the model is given
/// and what the session records, but it can be a whole attached document, and a stream that carried
/// it would be the caller paying twice for bytes it already has on disk. What a caller needs is not
/// the copy: it is the *fact* -- which files were inlined, where from, and how big -- so that a name
/// that matched nothing is visible as an empty list rather than as a model quietly answering about a
/// path. The key is absent rather than empty when nothing was inlined, so the frame for an ordinary
/// run stays exactly what it was.
pub fn turn_started_with(prompt: &str, attachments: &[crate::attach::Attachment]) -> String {
    if attachments.is_empty() {
        return turn_started(prompt);
    }
    let files: Vec<serde_json::Value> = attachments
        .iter()
        .map(|file| {
            json!({
                "token": file.token,
                "path": file.path.to_string_lossy(),
                "bytes": file.bytes,
                "lines": file.lines,
            })
        })
        .collect();
    frame(
        "turn.started",
        json!({ "prompt": prompt, "attachments": files }),
    )
}

/// The last line of a successful turn, carrying the tokens the provider last reported.
///
/// Zero when the provider never reported a count: a made-up number would be worse than an
/// honest zero, and `/usage` in the session file has the real one when there is one.
/// The structured answer of a run that was given a schema, once it has been checked.
///
/// Kept separate from `message.completed`, which is the answer as the model wrote it. This is the
/// answer the *caller* asked for: parsed, validated locally against the schema, and only ever
/// emitted when it passed. `attempts` is how many answers were asked for before one did -- 1 means
/// the first was accepted -- so a caller can see that a repair happened without diffing the turn
/// count. When the last attempt still fails, the stream gets an `error` and there is no `result`
/// line at all: a caller reading this type never sees a shape the schema does not describe.
pub fn result(json: &serde_json::Value, attempts: usize) -> String {
    serde_json::json!({"type": "result", "json": json, "attempts": attempts}).to_string()
}

/// How a turn ended, as the stream says it.
///
/// The exit code says this in one byte for a shell; this is for a program that reads the stream and
/// has to judge what the answer it just received is worth. Three values, and the reason there are
/// three is that a caller acting on the value has to tell them apart: an answer that is finished, an
/// answer that is unfinished because flint stopped asking, and a run the caller itself cut short.
/// Before this existed, the step limit was a `warning` string and nothing else -- so an unfinished
/// answer and a finished one were the same shape to a program, which is the kind of fault that shows
/// up as a wrong value rather than as an error. Which limit was reached is `Limit`, beside this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Outcome {
    #[default]
    Complete,
    Incomplete,
    Stopped,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Complete => "complete",
            Outcome::Incomplete => "incomplete",
            Outcome::Stopped => "stopped",
        }
    }
}

/// Which of flint's limits ended an unfinished turn.
///
/// `Outcome::Incomplete` says the answer is unfinished; this says *whose* budget ran out, and that is
/// the next question a caller has, because the two are fixed in different places: one is `max_steps`
/// in a config file, the other is `--max-seconds` on the command line. Without it the only difference
/// between them was the wording of a `warning`, which is the fault `outcome` itself was added to
/// remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Steps,
    Seconds,
}

impl Limit {
    pub fn as_str(self) -> &'static str {
        match self {
            Limit::Steps => "steps",
            Limit::Seconds => "seconds",
        }
    }
}

/// The end of a turn: what it cost, and what the answer is worth.
///
/// `limit` is carried only for an unfinished turn, and only when flint knows which budget ran out:
/// the key is absent rather than null everywhere else, so a caller reads its presence as the fact it
/// is.
///
/// `duration_ms` is always carried, and it is the one number here that is not about the answer. It
/// is measured from `turn.started` -- the frame the caller's wait begins at -- to this one, so it
/// answers "was that slow, or was it stuck" without the caller timing the subprocess, which would
/// also measure flint's own start-up and the caller's reading, and which a streaming caller cannot
/// do at all. What it is *not* is a cost: see B5 in `ROADMAP.md`, where the money half of this row
/// has no home because flint does not know what a token costs on the endpoint it was pointed at.
///
/// `provider_retries` is the other half of that question and always carried, unlike the two fields
/// below: a turn that took three tries and a turn that was merely slow look the same from
/// `duration_ms`, and a retry leaves no other mark on the stream -- it happens before anything has
/// been drawn, which is exactly why it is invisible. Zero is a fact here rather than a silence, so
/// the field is never absent and a caller never has to tell "no retries" from "nobody said".
pub fn turn_completed(
    usage: Option<Usage>,
    outcome: Outcome,
    limit: Option<Limit>,
    duration_ms: u64,
    provider_retries: u32,
) -> String {
    let mut body = json!({
        "prompt_tokens": usage.map_or(0, |u| u.prompt_tokens),
        "completion_tokens": usage.map_or(0, |u| u.completion_tokens),
        "outcome": outcome.as_str(),
        "duration_ms": duration_ms,
        "provider_retries": provider_retries,
    });
    // Like the `usage` frame's own field, and for the same reason: present only when the endpoint
    // reported a split, so a reader never sees a zero it would have to disbelieve.
    if let (Some(hit), Some(object)) = (usage.and_then(|u| u.cache_hit_tokens), body.as_object_mut()) {
        object.insert("cache_hit_tokens".to_string(), json!(hit));
    }
    if outcome == Outcome::Incomplete {
        if let (Some(limit), Some(object)) = (limit, body.as_object_mut()) {
            object.insert("reason".to_string(), json!(limit.as_str()));
        }
    }
    frame("turn.completed", body)
}

pub fn error(message: &str) -> String {
    frame("error", json!({ "message": message }))
}

/// A failure whose cause flint knows, told in the vocabulary a program reads.
///
/// `code` is the cause (`insufficient_balance`, `rate_limit`, `auth`, `no_key`, …) and `retryable` is
/// the question a caller actually has: may I try again, or must a person do something first. They are
/// absent on the plain [`error`], deliberately -- a frame with no classification is honest about
/// having none, where a `code: "unknown"` on every failure would look like knowledge.
///
/// This exists because the alternative was matching on the message text, which is what a caller had
/// to do to find out that an account was empty: three providers say that one fact three ways, and two
/// of them say it with a status that means something else as well.
pub fn error_coded(message: &str, code: &str, retryable: bool) -> String {
    frame(
        "error",
        json!({ "message": message, "code": code, "retryable": retryable }),
    )
}

pub fn warning(message: &str) -> String {
    frame("warning", json!({ "message": message }))
}

/// What a command answered, for the page's transcript.
///
/// Not one of the turn's events: a command runs *between* turns, and its answer is not part of
/// the conversation the session file holds -- which is exactly why it needs a line of its own
/// rather than a file the page can re-read. It is on the same stream, though, because the page
/// renders the transcript from that stream, and §8 puts an action's answer in the transcript.
///
/// The `input` is the line as it was typed: the page shows which command this answers, and a
/// person reading the transcript later needs it as much as the output.
pub fn command(input: &str, text: &str) -> String {
    frame("command", json!({ "input": input, "text": text }))
}

/// The same thing, asked for by the page rather than typed into the composer.
///
/// One line of vocabulary, not two: it *is* a command's answer, and the only difference is where the
/// reader wanted it -- the page keeps a report in its own panel, so §8 does not print it on the
/// terminal, and the page has to be told which of the two it is looking at. Hence a field on the
/// frame the page already handles rather than a new type: a page that did not know `panel` would
/// show the listing in the transcript, which is wrong but harmless, where a page that did not know a
/// new *type* would drop it in silence.
pub fn report(input: &str, text: &str) -> String {
    frame("command", json!({ "input": input, "text": text, "panel": true }))
}

/// What a turn is waiting for, for a reader that cannot see a terminal.
///
/// `restarted` is what makes this usable rather than merely present: a renderer showing
/// elapsed time has to start its clock over when a *new* wait begins and keep it running
/// when the same wait is renamed. The value comes from `Term`, which owns the clock and is
/// the only thing that knows the difference.
pub fn status(text: &str, restarted: bool) -> String {
    frame("status", json!({ "text": text, "restarted": restarted }))
}

/// The same frame on a clock, carrying how long the current wait has been going.
///
/// The gap this fills is not decoration. Everything else on this stream is emitted when something
/// *happens*, and between `tool.started` and `tool.completed` nothing happens for as long as the tool
/// runs -- so a slow command, a slow model and a crashed process are the same thing from a pipe:
/// silence. Measured: a six-second turn produced no line at all between `turn.started` and the
/// answer, and a caller waiting on that has no way to choose between waiting and killing.
///
/// `restarted` keeps the meaning it has in [`status`], so a renderer that runs its own clock is
/// unaffected, and `elapsed_secs` is for the readers that cannot run one: a consumer that joined the
/// stream late, or that is reading a log somebody else wrote. Two answers to the same question,
/// because the two readers are genuinely different.
pub fn heartbeat(text: &str, restarted: bool, elapsed_secs: u64) -> String {
    frame(
        "status",
        json!({ "text": text, "restarted": restarted, "elapsed_secs": elapsed_secs }),
    )
}

/// One line: the fields with `type` added.
///
/// `Value::to_string` is compact and escapes control characters, which is what makes the
/// "one line means one line" rule hold for any text a tool can produce. The keys come out
/// in `serde_json`'s own (alphabetical) order, which is stable across runs and versions --
/// worth more here than any particular field being first.
fn frame(kind: &str, fields: Value) -> String {
    let mut object = match fields {
        Value::Object(object) => object,
        _ => serde_json::Map::new(),
    };
    object.insert("type".to_string(), Value::String(kind.to_string()));
    Value::Object(object).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line is one line, whatever the text inside it looks like.
    ///
    /// This is the property the format rests on. A tool result is arbitrary bytes from a
    /// program -- newlines, quotes, tabs, a lone carriage return -- and if any of it were
    /// printed rather than escaped, a reader splitting on `\n` would see a fragment.
    #[test]
    fn a_text_event_is_one_line_however_the_text_is_shaped() {
        let mut sink = Sink::new();
        let text = "first\nsecond\r\nthird\ttab \"quoted\" back\\slash \u{1b}[31mred\u{1b}[0m";
        let line = sink.line(&Event::Text(text.to_string())).expect("a line");

        assert_eq!(line.matches('\n').count(), 0, "the line contains a newline");
        assert_eq!(line.matches('\r').count(), 0, "the line contains a return");

        let parsed: Value = serde_json::from_str(&line).expect("a line must be JSON");
        assert_eq!(parsed["type"], "message.delta");
        assert_eq!(
            parsed["text"], text,
            "the text did not survive the round trip"
        );
    }

    /// A result names the tool it came from, which only the earlier `ToolStart` knows.
    #[test]
    fn a_tool_result_names_its_tool() {
        let mut sink = Sink::new();
        sink.line(&Event::ToolStart {
            id: "call_1".to_string(),
            name: "read".to_string(),
        });
        let line = sink
            .line(&Event::ToolResult {
                id: "call_1".to_string(),
                output: "14 lines".to_string(),
                ok: true,
            })
            .expect("a line");
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["type"], "tool.completed");
        assert_eq!(parsed["name"], "read");
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["output"], "14 lines");
    }

    /// The answer is collected from fragments and delivered once, whole.
    #[test]
    fn the_finished_answer_is_one_line_of_the_whole_text() {
        let mut sink = Sink::new();
        for fragment in ["Hello", ", ", "world"] {
            sink.line(&Event::Text(fragment.to_string()));
        }
        let done: Value = serde_json::from_str(&sink.line(&Event::Done).unwrap()).unwrap();
        assert_eq!(done["type"], "message.completed");
        assert_eq!(done["text"], "Hello, world");

        // A second `Done` in the same turn does not repeat it.
        let again: Value = serde_json::from_str(&sink.line(&Event::Done).unwrap()).unwrap();
        assert_eq!(again["text"], "");
    }

    /// Every variant produces one parseable object with a `type`.
    #[test]
    fn every_line_is_an_object_that_says_what_it_is() {
        let mut sink = Sink::new();
        let events = vec![
            Event::Text("hi".to_string()),
            Event::Reasoning("thinking".to_string()),
            Event::ToolStart {
                id: "c1".to_string(),
                name: "bash".to_string(),
            },
            Event::ToolArgs {
                id: "c1".to_string(),
                args: "{\"command\":\"ls\"}".to_string(),
            },
            Event::ToolResult {
                id: "c1".to_string(),
                output: "a.txt".to_string(),
                ok: false,
            },
            Event::Usage(Usage {
                prompt_tokens: 10,
                completion_tokens: 4,
                cache_hit_tokens: None,
            }),
            Event::Warning("careful".to_string()),
            Event::Done,
        ];
        let mut kinds = Vec::new();
        for event in &events {
            let line = sink.line(event).expect("every event has a line");
            let parsed: Value = serde_json::from_str(&line).expect("a line must be JSON");
            let kind = parsed["type"]
                .as_str()
                .expect("every line has a type")
                .to_string();
            kinds.push(kind);
        }
        assert_eq!(
            kinds,
            vec![
                "message.delta",
                "reasoning.delta",
                "tool.started",
                "tool.args",
                "tool.completed",
                "usage",
                "warning",
                "message.completed",
            ]
        );
    }

    /// A text fragment is passed through as it arrives, so a consumer can stream it.
    #[test]
    fn deltas_are_passed_through_rather_than_held_back() {
        let mut sink = Sink::new();
        let first: Value = serde_json::from_str(
            &sink
                .line(&Event::Text("half".to_string()))
                .expect("a line"),
        )
        .unwrap();
        assert_eq!(first["text"], "half");
    }

    /// The same event writes the same bytes, so two runs can be compared with `diff`.
    #[test]
    fn the_same_event_writes_the_same_line_twice() {
        let event = Event::ToolResult {
            id: "call_9".to_string(),
            output: "one\ntwo".to_string(),
            ok: true,
        };
        let first = Sink::new().line(&event).expect("a line");
        let second = Sink::new().line(&event).expect("a line");
        assert_eq!(first, second);
        assert_eq!(
            first,
            r#"{"id":"call_9","name":null,"ok":true,"output":"one\ntwo","type":"tool.completed"}"#
        );
    }

    /// The frame lines carry the run's shape, with the session path as a string.
    #[test]
    fn the_frame_lines_say_where_and_what() {
        let started: Value = serde_json::from_str(&session_started(
            Some(Path::new("C:\\flint\\sessions\\x.jsonl")),
            Path::new("C:\\work"),
            "deepseek-chat",
        ))
        .unwrap();
        assert_eq!(started["type"], "session.started");
        assert_eq!(started["session"], "C:\\flint\\sessions\\x.jsonl");
        assert_eq!(started["cwd"], "C:\\work");
        assert_eq!(started["model"], "deepseek-chat");

        // No session (a run that could not create one) is null, not a missing key: a
        // consumer reads one shape, not two.
        let none: Value =
            serde_json::from_str(&session_started(None, Path::new("."), "m")).unwrap();
        assert!(none["session"].is_null());

        let usage: Value = serde_json::from_str(&turn_completed(
            Some(Usage {
                prompt_tokens: 7,
                completion_tokens: 3,
                cache_hit_tokens: None,
            }),
            Outcome::Complete,
            None,
            1234,
            0,
        ))
        .unwrap();
        assert_eq!(usage["type"], "turn.completed");
        assert_eq!(usage["prompt_tokens"], 7);
        // How long the turn took is on every ending, including the ones that are not an answer: a
        // stopped turn is exactly when a caller asks whether waiting was worth it.
        assert_eq!(usage["duration_ms"], 1234);
        // Always carried, and a zero here means the first attempt answered -- which is a fact, not a
        // field the endpoint stayed quiet about.
        assert_eq!(usage["provider_retries"], 0);
        // The outcome is a field on the end of the turn rather than a frame of its own: a consumer
        // that already reads this line for the token counts gets the answer's worth for free, and
        // there is no second thing to keep in step with the first.
        assert_eq!(usage["outcome"], "complete");
        assert_eq!(Outcome::Incomplete.as_str(), "incomplete");
        assert_eq!(Outcome::Stopped.as_str(), "stopped");
        // Which limit ran out is only said when one did: `reason` on an unfinished turn, and no key
        // at all on a finished one, so a caller reads its presence rather than a null.
        assert!(usage.get("reason").is_none(), "{usage}");
        let step_limited: Value = serde_json::from_str(&turn_completed(
            None,
            Outcome::Incomplete,
            Some(Limit::Steps),
            9,
            2,
        ))
        .unwrap();
        assert_eq!(step_limited["reason"], "steps");
        assert_eq!(
            step_limited["provider_retries"], 2,
            "an unfinished turn's retries are worth as much as a finished one's"
        );
        let timed_out: Value = serde_json::from_str(&turn_completed(
            None,
            Outcome::Incomplete,
            Some(Limit::Seconds),
            9,
            0,
        ))
        .unwrap();
        assert_eq!(timed_out["reason"], "seconds");
        let stopped: Value =
            serde_json::from_str(&turn_completed(None, Outcome::Stopped, None, 9, 1)).unwrap();
        assert!(stopped.get("reason").is_none(), "{stopped}");
        assert_eq!(stopped["provider_retries"], 1);

        let failed: Value = serde_json::from_str(&error("no model configured")).unwrap();
        assert_eq!(failed["type"], "error");
        assert_eq!(failed["message"], "no model configured");
    }
}
