//! What a turn's events become: the transcript, the status line, and the page's stream.
//!
//! This was the body of a closure inside `run_turn`, and `examples/live_turn.rs` kept a hand-written
//! copy of it. A copy of a rendering is a copy that stops matching: the example's transcript drifted
//! from the REPL's twice, and both times the fault was chased in the wrong file -- the example exists
//! to be *replayed* to judge a layout, so when it renders something the REPL does not, it invents
//! faults and hides real ones. There is one implementation now, and the example is a second caller of
//! it rather than a second version of it.
//!
//! What is deliberately *not* here: the interrupt and steering logic around a turn, which needs the
//! input channel and the future. This is only the part that turns one event into what a reader sees,
//! which is exactly the part both callers have to agree on.

use crate::display::{Printer, CYAN, RED};
use crate::event::Event;
use crate::web::Live;
use std::collections::HashMap;

/// The label the status line carries while the answer is arriving.
///
/// Kept in step with the REPL deliberately -- and now shared with the example for the same reason:
/// a label that differs between the two reads as a fault that does not exist.
pub const WRITING_LABEL: &str = "writing the answer";
/// And while the model is thinking out loud, which is not printed into the transcript.
pub const THINKING_LABEL: &str = "thinking";

/// One turn's rendering, from the first event to the last.
///
/// Built once per turn and fed every event the turn produces, in order. The only state it keeps is
/// what a *rendering* needs: which tool calls have been announced but not yet answered (so a result
/// can name what it was done to), the calls of the current round (so the clock can move to whatever is
/// being waited for now), and the answer so far (because a fragment is not a line and only the whole
/// text can be placed correctly).
pub struct EventSink<'a, 'p> {
    printer: &'a Printer<'p>,
    live: Option<&'a Live>,
    /// Announced calls, by id, with the name and the arguments they were given.
    tool_names: HashMap<String, (String, String)>,
    /// The calls of this round, in the order they were announced, so the clock can move to the next
    /// one as each result arrives.
    round_calls: Vec<String>,
    /// The answer so far, for the full redraw a terminal needs.
    answer: String,
    /// Whether any text has been streamed this round. Read by the caller afterwards: an answer that
    /// was on screen has to be ended, and a partial one has to be taken back.
    streamed_text: bool,
}

/// The wait, named. The clock keeps counting from the last `begin_round`.
///
/// A free function as well as a method, because a caller outside the turn -- the loop that escalates
/// a silent wait into "no response yet" -- cannot touch the sink while the turn's future holds it.
pub fn waiting(printer: &Printer<'_>, live: Option<&Live>, text: &str) {
    let restarted = printer.term().activity_started(text);
    announce_status(printer, live, restarted);
}

/// The same wait, named differently.
pub fn named(printer: &Printer<'_>, live: Option<&Live>, text: &str) {
    // Nothing running: naming it would invent an activity with no start time, and the browser must
    // not be told about a phase the terminal is not showing.
    if !printer.term().activity_named(text) {
        return;
    }
    announce_status(printer, live, false);
}

/// What the status row now says, to whoever else is rendering this run.
///
/// The words come from `Term::activity_label` and not from the caller's argument. They are not the
/// same thing: an unnamed wait is shown as `waiting for the model`, and a stream carrying the raw
/// empty name would blank the browser's status line at the exact moment the wait began -- which is
/// the whole thing this event exists to prevent. Called directly only when the terminal's status was
/// changed by something other than a wait -- clearing it, say.
pub fn announce_status(printer: &Printer<'_>, live: Option<&Live>, restarted: bool) {
    if let Some(live) = live {
        live.event(&Event::Status {
            text: printer.term().activity_label(),
            restarted,
        });
    }
}

impl<'a, 'p> EventSink<'a, 'p> {
    pub fn new(printer: &'a Printer<'p>, live: Option<&'a Live>) -> Self {
        Self {
            printer,
            live,
            tool_names: HashMap::new(),
            round_calls: Vec::new(),
            answer: String::new(),
            streamed_text: false,
        }
    }

    /// Start a model call: forget the round's tools, and put the clock on the request.
    ///
    /// Called once before the turn and once per tool round, because a tool round is another wait --
    /// the step starts by asking the model again, and the clock has to be running for it, or the pause
    /// after every tool call looks like the turn is over. `begin_answer` belongs here too: the
    /// terminal's record of what it has already committed is per model call, and measuring a new
    /// answer against the previous round's text is how a streamed answer comes out cut in half.
    pub fn begin_round(&mut self) {
        self.tool_names.clear();
        self.round_calls.clear();
        self.answer.clear();
        self.streamed_text = false;
        self.printer.term().begin_answer();
        self.waiting("");
    }

    /// Whether this round has streamed any text.
    pub fn streamed(&self) -> bool {
        self.streamed_text
    }

    /// The wait, named. The clock keeps counting from `begin_round`.
    pub fn waiting(&mut self, text: &str) {
        waiting(self.printer, self.live, text);
    }

    /// The same wait, named differently.
    pub fn named(&mut self, text: &str) {
        named(self.printer, self.live, text);
    }

    /// One event of the turn.
    pub fn event(&mut self, event: Event) {
        // A phase change is announced *before* the thing that caused it, so a reader of the stream sees
        // the two in the order the terminal does: `writing the answer`, then the text. Emitting the
        // event first and the status from inside its arm put them the wrong way round, which the first
        // live capture showed immediately. `named` is idempotent, so repeating this for every fragment
        // of one answer costs nothing.
        match &event {
            Event::Text(_) if !self.streamed_text => self.named(WRITING_LABEL),
            // The same two guards the arm below used to carry, kept together with the announcement
            // they belong to: a fragment arriving after the answer has started, or a blank one, is not
            // a phase.
            Event::Reasoning(t) if !self.streamed_text && !t.trim().is_empty() => {
                self.named(THINKING_LABEL)
            }
            _ => {}
        }
        // Every event the turn produces, on the browser's stream as well. This is the one place they
        // all pass, which is what makes the browser a renderer of the same run rather than a
        // reconstruction of it.
        if let Some(live) = self.live {
            live.event(&event);
        }
        match event {
            Event::Text(t) => {
                if self.printer.term().interactive() {
                    // The answer is redrawn in full, so the terminal always shows a complete line even
                    // when a fragment stops mid-word.
                    self.answer.push_str(&t);
                    self.printer.term().stream(&self.answer);
                } else {
                    // A pipeline gets each fragment once. Re-rendering here would print the whole
                    // answer again per fragment.
                    self.printer.term().stream(&t);
                }
                self.streamed_text = true;
            }
            Event::Reasoning(_) => {
                // The model thinking out loud.
                //
                // Not put in the transcript: the provider emits one event per SSE fragment, so there is
                // no natural place to break the block into lines, and the whole reasoning spread down
                // the screen is what a live turn used to look like. It is not thrown away either -- it
                // is in the session file.
                //
                // What the user needs from it is *that it is happening*, and that belongs on the status
                // line, which is already showing how long the turn has taken. A separate `… thinking`
                // marker in the transcript said the same thing a second time, one row above, and left
                // the status line claiming the model was still being waited for while it was in fact
                // already talking. The label is put up by the match above, before this event reaches
                // the stream.
            }
            Event::ToolStart { id, name } => {
                // Start the clock as soon as the tool is known, not when its arguments have finished
                // streaming: the wait begins here, and a tool that never returns is exactly the case
                // this is for.
                //
                // Only for the first call of the round, though. Every call in a round is announced
                // before any of them runs, so naming the clock on each announcement claimed the *last*
                // call was the one being waited for, and reset the elapsed time of a tool that had not
                // started yet.
                if self.round_calls.is_empty() {
                    self.waiting(&name);
                }
                self.round_calls.push(id.clone());
                self.tool_names.insert(id, (name, String::new()));
            }
            Event::ToolArgs { id, args } => {
                let name = self
                    .tool_names
                    .get(&id)
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default();
                self.printer.tool_call(&name, &args);
                // Remembered so the result line can name what it was done to. Without it, two reads of
                // two different files both print "read N lines" and the transcript looks like a
                // duplicate.
                if let Some(entry) = self.tool_names.get_mut(&id) {
                    entry.1 = args;
                }
            }
            Event::ToolResult { id, output, ok } => {
                // The name comes from the matching ToolStart: the result is reported as "✔ read" or
                // "✔ bash", so the transcript says what happened rather than just that something did.
                let (name, args) = self
                    .tool_names
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| (String::new(), String::new()));
                self.printer.tool_result(&name, &args, &output, ok);
                self.tool_names.remove(&id);
                self.round_calls.retain(|call| call != &id);
                // The clock belongs to whatever is being waited for now: the next call in the round,
                // or -- once they have all run -- the model that has to be asked for the round after
                // this one. Clearing it here instead left the rest of a multi-call round and the whole
                // following model call with no clock at all, which is the pause a user actually stares
                // at.
                let next = self
                    .round_calls
                    .first()
                    .and_then(|next| self.tool_names.get(next))
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default();
                self.waiting(&next);
            }
            Event::Usage(_) => {}
            Event::Warning(w) => {
                self.printer.term().blank();
                self.printer
                    .term()
                    .line(format_args!("{} {w}", self.printer.style(RED, "warning:")));
            }
            // Shown, never sent. Two lines because the second one is the whole safety rule, and a
            // person reading a message that sounds like an instruction needs to know that the model
            // has not seen it -- otherwise "the agent ignored me" is the natural, wrong conclusion.
            Event::Peer { from, text } => {
                self.printer.term().blank();
                self.printer.term().line(format_args!(
                    "{} {} says: {text}",
                    self.printer.style(CYAN, "peer"),
                    if from.trim().is_empty() { "someone" } else { from.as_str() }
                ));
                self.printer.term().line(format_args!(
                    "      (shown to you; not sent to the model)"
                ));
            }
            Event::Done => {}
            // The terminal never receives one of these: the status *is* the terminal's own row, and
            // `waiting` reads from it rather than feeding it. A caller that could send one would be a
            // second place deciding the phase.
            Event::Status { .. } => {}
        }
    }
}
