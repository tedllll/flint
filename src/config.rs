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

    /// Optional proxy for commands run by the `bash` tool and `flint exec`,
    /// e.g. `http://127.0.0.1:10808`. When set, it is exported to the child
    /// process as HTTP_PROXY / HTTPS_PROXY / ALL_PROXY (both cases).
    ///
    /// This matters more than it looks: the whole point of flint is to repair
    /// tooling, and most repairs download something. On a network where direct
    /// access is blocked but a local proxy works, a flint that cannot pass the
    /// proxy along is useless exactly when it is needed.
    ///
    /// Serialised as a plain string (empty = off) so the option is visible in
    /// the generated file. A field that vanishes when unset is a field nobody
    /// discovers.
    #[serde(
        default,
        serialize_with = "ser_opt_string",
        deserialize_with = "de_opt_string"
    )]
    pub proxy: Option<String>,

    pub providers: Vec<ProviderConfig>,
}

fn default_max_tool_output() -> usize {
    30_000
}

fn default_max_steps() -> usize {
    25
}

/// Serialise `Option<String>` as a plain string, `None` becoming `""`.
///
/// `toml` cannot represent `None`, so the default behaviour is to drop the key
/// entirely -- which hides the option from anyone reading the generated config.
fn ser_opt_string<S: serde::Serializer>(
    value: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.as_deref().unwrap_or(""))
}

/// Read back the plain-string form: `""` means unset.
fn de_opt_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.filter(|s| !s.trim().is_empty()))
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
            proxy: None,
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
    /// Add a provider, or overwrite the one with the same name.
    ///
    /// Returns true when a new provider was added, false when an existing one
    /// was updated. This is what makes `flint` configurable from inside itself:
    /// a rescue tool that requires you to hand-edit TOML before it will talk to
    /// anything has already failed at the first step.
    pub fn upsert_provider(&mut self, p: ProviderConfig) -> bool {
        match self.providers.iter_mut().find(|x| x.name == p.name) {
            Some(slot) => {
                *slot = p;
                false
            }
            None => {
                self.providers.push(p);
                true
            }
        }
    }

    pub fn remove_provider(&mut self, name: &str) -> bool {
        let before = self.providers.len();
        self.providers.retain(|p| p.name != name);
        self.providers.len() != before
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

    /// The provider that should be used when nothing overrides it.
    ///
    /// Falls back to the first configured provider if `default_provider` names
    /// something that no longer exists: refusing to start would strand the user
    /// in a tool whose only job is to get them unstuck.
    pub fn fallback_provider(&self) -> Option<&ProviderConfig> {
        self.provider(&self.default_provider)
            .or_else(|| self.providers.first())
    }

    /// Load the config, creating a default file on first run.
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
}
