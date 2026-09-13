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

    /// The line for an event, or `None` for the ones that are already fully described by
    /// another event.
    pub fn line(&mut self, event: &Event) -> Option<String> {
        Some(match event {
            Event::Text(text) => {
                self.answer.push_str(text);
                frame("message.delta", json!({ "text": text }))
            }
            Event::Reasoning(text) => frame("reasoning.delta", json!({ "text": text })),
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
            Event::Usage(usage) => frame(
                "usage",
                json!({
                    "prompt_tokens": usage.prompt_tokens,
                    "completion_tokens": usage.completion_tokens,
                    "total_tokens": usage.total(),
                }),
            ),
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

/// The last line of a successful turn, carrying the tokens the provider last reported.
///
/// Zero when the provider never reported a count: a made-up number would be worse than an
/// honest zero, and `/usage` in the session file has the real one when there is one.
pub fn turn_completed(usage: Option<Usage>) -> String {
    frame(
        "turn.completed",
        json!({
            "prompt_tokens": usage.map_or(0, |u| u.prompt_tokens),
            "completion_tokens": usage.map_or(0, |u| u.completion_tokens),
        }),
    )
}

pub fn error(message: &str) -> String {
    frame("error", json!({ "message": message }))
}

pub fn warning(message: &str) -> String {
    frame("warning", json!({ "message": message }))
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

        let usage: Value = serde_json::from_str(&turn_completed(Some(Usage {
            prompt_tokens: 7,
            completion_tokens: 3,
        })))
        .unwrap();
        assert_eq!(usage["type"], "turn.completed");
        assert_eq!(usage["prompt_tokens"], 7);

        let failed: Value = serde_json::from_str(&error("no model configured")).unwrap();
        assert_eq!(failed["type"], "error");
        assert_eq!(failed["message"], "no model configured");
    }
}
