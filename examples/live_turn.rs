// Drives the interactive layout against a real provider, so the transcript can be
// replayed from genuine model output rather than a fixture.
//
// `flint` is called exactly the way the REPL calls it, and the bytes go to stdout
// like any interactive run. Use `FLINT_TERM_CAPTURE=1` to force the interactive
// branches in a debug build; the caller redirects stdout to a file and replays it
// with scripts/vtscreen.js.
//
//   cargo run --example live_turn -- "帮我看看当前目录有几个文件"
use flint::agent::Agent;
use flint::config::Config;
use flint::event::Event;
use flint::provider::Provider;
use flint::term::Term;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let question = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "用一句话回答：1+1 等于几".to_string());

    let cfg = Config::load()?;
    let name = cfg.default_provider.clone();
    let provider = cfg
        .provider(&name)
        .ok_or_else(|| anyhow::anyhow!("provider '{name}' is not configured"))?
        .clone();

    let term = Term::start()?;
    let cwd = std::env::current_dir()?;
    // NORMAL, not the config's setting: this example is for judging the default
    // transcript, and CHATTY deliberately shows more of each tool result. Set
    // FLINT_LIVE_VERBOSE=1 to see the chatty layout instead.
    let verbosity = if std::env::var_os("FLINT_LIVE_VERBOSE").is_some() {
        flint::display::CHATTY
    } else {
        flint::display::NORMAL
    };
    let printer = flint::display::Printer::new(false, verbosity, &term);
    // Read-only, so a layout check can never run a command by accident.
    let mut agent = Agent::new(&cfg, Provider::new(provider)?, true, cwd, None);

    // The REPL echoes the question before the turn, so the layout test covers it.
    printer.term().line(format_args!("> {question}"));

    let mut answer = String::new();
    let mut names: std::collections::HashMap<String, (String, String)> = std::collections::HashMap::new();
    // Same rule as the REPL: the reasoning channel fires once per token, so the
    // turn shows one marker per turn rather than a word per line.
    let mut thinking_shown = false;
    agent
        .run(&question, |event| match event {
            Event::Text(t) => {
                answer.push_str(&t);
                printer.term().stream(&answer);
            }
            Event::Reasoning(t) => {
                if !thinking_shown && !t.trim().is_empty() {
                    thinking_shown = true;
                    printer
                        .term()
                        .line(format_args!("{}", printer.dim("\u{2026} thinking")));
                }
            }
            Event::ToolStart { id, name } => {
                names.insert(id, (name, String::new()));
            }
            Event::ToolArgs { id, args } => {
                let name = names.get(&id).map(|(n, _)| n.clone()).unwrap_or_default();
                printer.tool_call(&name, &args);
                if let Some(entry) = names.get_mut(&id) {
                    entry.1 = args;
                }
            }
            Event::ToolResult { id, output, ok } => {
                let (name, args) = names
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| (String::new(), String::new()));
                // Goes through the printer, not the terminal: this is the code path
                // under test, and writing straight to the term would bypass it.
                printer.tool_result(&name, &args, &output, ok);
                names.remove(&id);
            }
            _ => {}
        })
        .await?;
    printer.term().end_stream();
    printer.term().blank();
    printer.term().line(format_args!("[done]"));

    Ok(())
}
