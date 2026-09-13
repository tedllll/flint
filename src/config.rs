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
    /// Other models this provider can serve, for `/model` to offer.
    ///
    /// An empty list is the common case and means "only `model`": one endpoint serving
    /// one model needs no menu, and a config written before this field existed keeps
    /// working unchanged. `model` is always among the choices, so it does not have to be
    /// repeated here.
    #[serde(default)]
    pub models: Vec<String>,
    /// Read the key from this environment variable instead of `api_key`.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Proxy for reaching *this provider*, e.g. `socks5h://127.0.0.1:10808` or
    /// `http://127.0.0.1:10808`.
    ///
    /// Separate from [`Config::proxy`], which is for the `bash` tool's child processes:
    /// the model endpoint and the commands it asks for can need different routes, and a
    /// provider reached through a dead proxy must be fixable without disturbing them.
    ///
    /// Empty or absent means "ask the environment, then the system setting".
    #[serde(default, deserialize_with = "de_opt_string")]
    pub proxy: Option<String>,
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

    /// How much of the agent's own activity to print.
    ///
    /// `false` (the default) prints one compact line per tool call plus a short
    /// result. `true` adds the tool's arguments, and the reasoning marker.
    #[serde(default)]
    pub verbose: bool,

    /// Whether to print the output behind a tool result.
    ///
    /// Off by default, and separate from `verbose` on purpose. A directory listing
    /// is forty lines and reading a file is hundreds; wanting a running commentary
    /// on what the model is doing is not the same as wanting the file it read
    /// printed into the conversation. `/verbose full` and this used to be the same
    /// switch, which meant the commentary could not be turned up without the output
    /// coming with it.
    #[serde(default)]
    pub tool_detail: bool,

    /// What to do with project instruction files (`AGENTS.md`): `hint`, `paste` or `off`.
    ///
    /// Named rather than pasted by default, because naming is what the model can act on:
    /// it reads the file with the `read` tool, so the prompt does not carry a document
    /// that changes, and a long one does not spend context on every request. `paste`
    /// exists for anyone who wants the contents in the prompt itself.
    #[serde(default = "default_instructions")]
    pub instructions: String,

    /// Extra directories to look for skills in, on top of the standard two
    /// (`<project>/.flint/skills` and the config directory's `skills`).
    ///
    /// Relative paths are resolved against the working directory, so a shared checkout
    /// of house procedures can be named once and used from anywhere.
    #[serde(default)]
    pub skill_dirs: Vec<String>,

    pub providers: Vec<ProviderConfig>,
}

fn default_instructions() -> String {
    crate::context::Instructions::DEFAULT_NAME.to_string()
}

fn default_max_tool_output() -> usize {
    30_000
}

/// Steps in one turn, counting model round-trips rather than tool runs, so a turn that
/// calls two tools at a time gets two calls per step.
///
/// Generous on purpose. Reaching this is not a safety net doing its job, it is a turn
/// that was cut off in the middle of the work it was asked to do -- and the person
/// watching cannot tell how close it was until it happens. It exists for the runaway
/// case (a model looping on the same failing command), not to ration work. An ordinary
/// turn takes five to fifteen steps; a real repair, reading files and running builds,
/// takes thirty or more.
fn default_max_steps() -> usize {
    100
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

    /// The models `/model` can offer for this provider.
    ///
    /// Always includes the active `model`, so it appears in the list whether or not it
    /// was repeated in `models` -- and a provider with no `models` list still offers the
    /// one it is using, which keeps a config written before the field existed usable.
    /// Deduplicated in order, because the active model may well be listed too.
    pub fn choices(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if !self.model.trim().is_empty() {
            out.push(self.model.clone());
        }
        for m in &self.models {
            let m = m.trim();
            if !m.is_empty() && !out.iter().any(|x| x == m) {
                out.push(m.to_string());
            }
        }
        out
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
            verbose: false,
            tool_detail: false,
            instructions: default_instructions(),
            skill_dirs: Vec::new(),
            providers: vec![
                ProviderConfig {
                    name: "deepseek".to_string(),
                    base_url: "https://api.deepseek.com/v1".to_string(),
                    api_key: String::new(),
                    model: "deepseek-chat".to_string(),
                    models: vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()],
                    api_key_env: Some("DEEPSEEK_API_KEY".to_string()),
                    proxy: None,
                },
                // A local fallback costs nothing to configure and still works
                // when every hosted provider is unreachable.
                ProviderConfig {
                    name: "ollama".to_string(),
                    base_url: "http://localhost:11434/v1".to_string(),
                    api_key: "ollama".to_string(),
                    model: "qwen2.5-coder:7b".to_string(),
                    models: Vec::new(),
                    api_key_env: None,
                    proxy: None,
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

/// Where flint keeps its config and sessions.
///
/// `FLINT_HOME` overrides it, for two reasons. A test needs a directory of its own
/// rather than the developer's real config -- and this is a rescue tool, so "start with
/// a different config file without touching the one that is broken" is a use, not a
/// testing convenience.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FLINT_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    home_dir().join(".flint")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}

/// Where tool output too long for one request is kept, in full, as plain text.
///
/// Under the session rather than in a single pile, because the only question anyone asks
/// of these files is "what did that command actually print", and the answer belongs to one
/// conversation. Nothing reads them back automatically: they are there for a person, or
/// for the model, to `read` deliberately.
pub fn spill_dir() -> PathBuf {
    config_dir().join("spill")
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

    /// Load the config if it exists, without creating one and without printing anything.
    ///
    /// This is what `exec` uses, and the difference matters more than it looks. `exec` is
    /// the last line of defence: it must run a command when every provider is
    /// unreachable, which is exactly the situation where the config may be missing,
    /// half-written, or damaged. It therefore must not create a config file as a side
    /// effect, and must not fail because an existing one cannot be parsed -- neither is
    /// the user's problem when all they asked for was `exec echo hi`.
    ///
    /// A config that cannot be read falls back to defaults rather than erroring: the only
    /// thing it contributes to `exec` is which shell to use, and a default shell beats no
    /// command at all.
    pub fn load_existing() -> Self {
        let path = config_path();
        if !path.exists() {
            return Config::default();
        }
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
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
        cfg.validate()?;
        Ok(cfg)
    }

    /// Reject a config that names something flint does not know.
    ///
    /// Checked when the file is read, not when the value is used, so a typo is reported
    /// once with the valid values in the message -- instead of quietly behaving like the
    /// default and surfacing later as "the model ignored my instruction file".
    fn validate(&self) -> Result<()> {
        if crate::context::Instructions::parse(&self.instructions).is_none() {
            return Err(anyhow::anyhow!(
                "unknown instructions mode '{}' in {}.\n\
                 Use \"hint\" (name the instruction files), \"paste\" (put their contents \
                 in the prompt) or \"off\".",
                self.instructions,
                config_path().display()
            ));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A file written before instruction files existed must keep working, and must get the
    /// default mode rather than an empty string that means nothing.
    #[test]
    fn an_old_config_without_the_new_keys_still_loads() {
        let cfg: Config = toml::from_str(
            "default_provider = \"deepseek\"\n\n\
             [[providers]]\n\
             name = \"deepseek\"\n\
             base_url = \"https://example.invalid/v1\"\n",
        )
        .expect("an older config must still parse");
        assert_eq!(cfg.instructions, "hint");
        assert!(cfg.skill_dirs.is_empty());
        cfg.validate().expect("the default mode is valid");
    }

    /// A typo is reported with the values that would work, when the file is read -- rather
    /// than being discovered later as "the model ignored my instruction file".
    #[test]
    fn an_unknown_instruction_mode_is_rejected_by_name() {
        let cfg = Config {
            instructions: "sometimes".to_string(),
            ..Config::default()
        };
        let err = format!("{:#}", cfg.validate().unwrap_err());
        assert!(err.contains("sometimes"), "{err}");
        for mode in ["hint", "paste", "off"] {
            assert!(err.contains(mode), "the error must offer {mode}: {err}");
        }
    }
}
