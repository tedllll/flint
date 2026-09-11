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
    // Read-only, so a layout check can never run a command by accident.
    let mut agent = Agent::new(&cfg, Provider::new(provider)?, true, cwd, None);

    // The REPL echoes the question before the turn, so the layout test covers it.
    term.line(format_args!("> {question}"));

    let mut answer = String::new();
    let mut names: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    agent
        .run(&question, |event| match event {
            Event::Text(t) => {
                answer.push_str(&t);
                term.stream(&answer);
            }
            Event::Reasoning(t) => {
                if !t.trim().is_empty() {
                    term.line(format_args!("[思考] {}", t.trim()));
                }
            }
            Event::ToolStart { id, name } => {
                names.insert(id, name);
            }
            Event::ToolArgs { id, args } => {
                let name = names.get(&id).cloned().unwrap_or_default();
                term.line(format_args!("  · {name} {args}"));
            }
            Event::ToolResult { output, ok, .. } => {
                let mark = if ok { "✓" } else { "✗" };
                term.line(format_args!("  {mark} {output}"));
            }
            _ => {}
        })
        .await?;
    term.end_stream();
    term.blank();
    term.line(format_args!("[done]"));

    Ok(())
}
