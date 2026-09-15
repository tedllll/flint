// Drives the interactive layout against a real provider, so the transcript can be
// replayed from genuine model output rather than a fixture.
//
// `flint` is called exactly the way the REPL calls it, and the bytes go to stdout
// like any interactive run. Use `FLINT_TERM_CAPTURE=1` to force the interactive
// branches in a debug build; the caller redirects stdout to a file and replays it
// with scripts/vtscreen.js.
//
//   cargo run --example live_turn -- "帮我看看当前目录有几个文件"
//
// The rendering is the REPL's own, through `sink::EventSink`: this file used to keep a
// hand-written copy of `run_turn`'s event handling, and the copy drifted twice, so a layout
// fault got chased in the example and a fault in the example looked like the REPL's. An
// example that renders something the REPL does not is worse than no example -- this one exists
// so a layout can be *judged*, and judging it against a second implementation judges the wrong
// thing.
use flint::agent::Agent;
use flint::config::Config;
use flint::display::Printer;
use flint::provider::Provider;
use flint::sink::EventSink;
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
    // Same as the REPL: without this, paths in the transcript keep their full prefix and
    // this example stops reproducing what the REPL actually shows.
    term.set_cwd(&cwd.to_string_lossy());
    // NORMAL, not the config's setting: this example is for judging the default
    // transcript, and CHATTY deliberately shows more of each tool result. Set
    // FLINT_LIVE_VERBOSE=1 to see the chatty layout instead.
    let verbosity = if std::env::var_os("FLINT_LIVE_VERBOSE").is_some() {
        flint::display::CHATTY
    } else {
        flint::display::NORMAL
    };
    let printer = Printer::new(false, verbosity, &term);
    // Read-only, so a layout check can never run a command by accident.
    let mut agent = Agent::new(&cfg, Provider::new(provider)?, true, cwd, None);

    // The REPL echoes the question before the turn, so the layout test covers it.
    printer.term().line(format_args!("> {question}"));

    // No browser: this is the terminal's rendering of the turn and nothing else. The REPL
    // would hand the same sink its `--web` feed, which is why the page can be a renderer of a
    // run rather than a reconstruction of one.
    let mut sink = EventSink::new(&printer, None);
    sink.begin_round();
    agent.run(&question, |event| sink.event(event)).await?;
    if sink.streamed() {
        printer.term().end_stream();
    }
    printer.term().blank();
    printer.term().line(format_args!("[done]"));

    Ok(())
}
