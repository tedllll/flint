// Print the raw event stream for one turn: which channel each fragment arrives on.
//
// The live transcript showed the model's "I'll run some network diagnostics" line
// repeated once per tool round. That sentence should arrive as reasoning and be
// collapsed to a single marker, so either the provider is classifying it as content
// or the strip is keeping stale text between segments. This distinguishes the two
// without guessing.
//
//   FLINT_LIVE_VERBOSE=1 cargo run --example channels -- "your question"
use flint::agent::Agent;
use flint::event::Event;
use flint::config::Config;
use flint::provider::Provider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let question = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "测试连通性".to_string());
    let cfg = Config::load()?;
    let name = cfg.default_provider.clone();
    let provider = cfg
        .provider(&name)
        .ok_or_else(|| anyhow::anyhow!("provider '{name}' is not configured"))?
        .clone();

    let cwd = std::env::current_dir()?;
    let mut agent = Agent::new(&cfg, Provider::new(provider)?, true, cwd, None);

    let mut channel = "";
    let (mut text, mut reasoning) = (0usize, 0usize);
    agent
        .run(&question, |event| match event {
            Event::Text(t) => {
                if channel != "text" {
                    println!("\n=== CONTENT ===");
                    channel = "text";
                }
                text += 1;
                print!("{t}");
            }
            Event::Reasoning(t) => {
                if channel != "reasoning" {
                    println!("\n=== REASONING ===");
                    channel = "reasoning";
                }
                reasoning += 1;
                print!("{t}");
            }
            Event::ToolArgs { args, .. } => println!("\n[tool args] {args}"),
            Event::ToolResult { output, ok, .. } => {
                println!("\n[tool result ok={ok}] {} lines", output.lines().count());
                channel = "";
            }
            Event::Warning(w) => println!("\n[warning] {w}"),
            _ => {}
        })
        .await?;

    println!(
        "\n\n--- totals: {} content fragments, {} reasoning fragments ---",
        text, reasoning
    );
    Ok(())
}
