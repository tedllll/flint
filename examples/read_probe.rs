// Print exactly what the `read` tool returns, with escape/whitespace made visible.
//
// The transcript showed the read output arriving as scattered digits, which could
// be the tool's own formatting, the display layer, or the replay emulator. This
// removes the middlemen: it calls the tool and prints the bytes.
//
//   cargo run --example read_probe -- Cargo.toml
use flint::config::Config;
use flint::tools::ToolBox;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "Cargo.toml".into());
    let cfg = Config::load()?;
    let cwd = std::env::current_dir()?;
    let tools = ToolBox::new(&cfg, true, cwd);

    let args: serde_json::Value = serde_json::json!({ "path": path });
    let out = tools.invoke("read", &args).await?;

    println!("--- {} lines from read ---", out.lines().count());
    for (n, line) in out.lines().enumerate() {
        if n >= 12 {
            break;
        }
        // Tabs, carriage returns and trailing spaces are the suspects, so show them.
        let visible = line.replace('\t', "<TAB>").replace('\r', "<CR>");
        println!("{n:>2}|{visible}|");
    }
    Ok(())
}
