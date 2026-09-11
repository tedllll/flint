//! Plain-text configuration.
//!
//! Deliberately a hand-editable TOML file in the user's home directory. When
//! things are broken you may not have a working model to fix this for you, so
//! the file must be repairable with `notepad` / `vi`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Short identifier used on the command line (`--provider deepseek`).
    pub name: String,
    /// OpenAI-compatible base URL. The client appends `/chat/completions`
    /// unless the URL already ends with it.
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    /// Read the key from this environment variable instead of `api_key`.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Which provider is used when `--provider` is not given.
    pub default_provider: String,

    /// Shell program used by the `bash` tool. This is the *program only*:
    ///   Unix:    "sh"     (falls back to bash/zsh)
    ///   Windows: "cmd"    (falls back to powershell/pwsh)
    #[serde(default = "default_shell")]
    pub shell: String,

    /// Arguments that make the shell execute a command string and exit.
    ///   Unix:    ["-c"]
    ///   Windows: ["/C"]
    #[serde(default = "default_shell_args")]
    pub shell_args: Vec<String>,

    /// Hard cap on tool output characters fed back into the model.
    #[serde(default = "default_max_tool_output")]
    pub max_tool_output: usize,

    /// Safety valve against a runaway loop.
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,

    /// Refuse every write/edit/bash while set. Toggle with `/readonly`.
    #[serde(default)]
    pub readonly: bool,

    pub providers: Vec<ProviderConfig>,
}

fn default_max_tool_output() -> usize {
    30_000
}

fn default_max_steps() -> usize {
    25
}

impl ProviderConfig {
    /// Resolve the effective API key, preferring the environment variable.
    pub fn resolved_key(&self) -> String {
        if let Some(var) = &self.api_key_env {
            if let Ok(v) = std::env::var(var) {
                if !v.trim().is_empty() {
                    return v;
                }
            }
        }
        self.api_key.clone()
    }

    /// Full chat-completions endpoint.
    pub fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_provider: "deepseek".to_string(),
            shell: default_shell(),
            shell_args: default_shell_args(),
            max_tool_output: default_max_tool_output(),
            max_steps: default_max_steps(),
            readonly: false,
            providers: vec![
                ProviderConfig {
                    name: "deepseek".to_string(),
                    base_url: "https://api.deepseek.com/v1".to_string(),
                    api_key: String::new(),
                    model: "deepseek-chat".to_string(),
                    api_key_env: Some("DEEPSEEK_API_KEY".to_string()),
                },
                // A local fallback costs nothing to configure and still works
                // when every hosted provider is unreachable.
                ProviderConfig {
                    name: "ollama".to_string(),
                    base_url: "http://localhost:11434/v1".to_string(),
                    api_key: "ollama".to_string(),
                    model: "qwen2.5-coder:7b".to_string(),
                    api_key_env: None,
                },
            ],
        }
    }
}

fn default_shell() -> String {
    if cfg!(windows) {
        "cmd".to_string()
    } else {
        "sh".to_string()
    }
}

fn default_shell_args() -> Vec<String> {
    if cfg!(windows) {
        vec!["/C".to_string()]
    } else {
        vec!["-c".to_string()]
    }
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    home_dir().join(".flint")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}

impl Config {
    /// Load the config, creating a commented default file on first run.
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            let cfg = Config::default();
            cfg.save()?;
            eprintln!(
                "flint: created default config at {}\n\
                 flint: set your API key there (or export DEEPSEEK_API_KEY), then re-run.\n",
                path.display()
            );
            return Ok(cfg);
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read config {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("cannot parse config {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(config_dir()).context("cannot create config directory")?;
        let body = toml::to_string_pretty(self).context("cannot serialize config")?;
        let header = "\
# flint configuration.
# `base_url` must be OpenAI-compatible: POST {base_url}/chat/completions
# Add as many providers as you like and switch with --provider <name> or /provider.

";
        let path = config_path();
        std::fs::write(&path, format!("{header}{body}"))
            .with_context(|| format!("cannot write config {}", path.display()))?;
        Ok(())
    }

    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// The provider to use, honouring an explicit override.
    pub fn active_provider(&self, override_name: Option<&str>) -> Result<&ProviderConfig> {
        let name = override_name.unwrap_or(&self.default_provider);
        self.provider(name).ok_or_else(|| {
            let known: Vec<&str> = self.providers.iter().map(|p| p.name.as_str()).collect();
            anyhow::anyhow!(
                "unknown provider '{}'. configured providers: {}",
                name,
                if known.is_empty() {
                    "(none)".to_string()
                } else {
                    known.join(", ")
                }
            )
        })
    }
}
