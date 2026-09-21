//! Plain-text configuration.
//!
//! Deliberately a hand-editable TOML file in the user's home directory. When
//! things are broken you may not have a working model to fix this for you, so
//! the file must be repairable with `notepad` / `vi`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    /// A command that starts this provider's engine, run when flint needs the provider and
    /// nothing is answering at `base_url`.
    ///
    /// For a local model server and nothing else. An endpoint on the internet is either
    /// reachable or not, and starting it is not this machine's business; a local one is a
    /// program that has to be running before the endpoint exists at all. See
    /// [`crate::engine`] for why flint runs a command instead of owning a process.
    #[serde(default)]
    pub start: Option<String>,

    /// A command that stops it, run when a switch leaves this provider behind.
    ///
    /// Absent means the engine is left running: a model somebody else is also using — an
    /// ollama serving a GUI, say — must not be shut down because a conversation moved on.
    #[serde(default)]
    pub stop: Option<String>,

    /// How long to wait for the endpoint after `start`. Zero means the default.
    ///
    /// Worth raising for a large model on a cold disk, where the honest answer is minutes.
    #[serde(default)]
    pub start_timeout_secs: u64,

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

    /// The JSON field to put a reasoning level in, for this endpoint.
    ///
    /// A field *name* rather than a level, and empty by default, because this area has no standard
    /// at all: the value is flint's (`low`, `medium`, `high`) and the field is the vendor's. Pi
    /// keeps a compatibility table for this -- `reasoning_effort`, `openrouter`, `deepseek`,
    /// `together`, `qwen`, `chat-template` -- with its own comment that "Grok models don't like
    /// `reasoning_effort`", and a table like that is a list of other people's servers to keep in
    /// step with. `reasoning_effort` is the common one. Empty means this provider is never sent a
    /// reasoning parameter, whatever level the run is at: a field flint guessed wrong is a request
    /// an endpoint may refuse outright, which reads as flint being broken.
    #[serde(default)]
    pub thinking_field: String,
}

/// Web search: where a `search` tool gets its answers.
///
/// Optional, and normally absent. With no block at all, flint inherits the credential of a
/// provider pointed at DeepSeek -- the key is taken exactly the way that provider takes it,
/// which is what "configured once, used twice" should mean. The block exists for the case
/// that has no such provider: a machine running only a local model, where the search is a
/// separate service with its own key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// `false` turns search off even when a credential could be inherited.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Take the key (and the proxy) from this provider, by name.
    ///
    /// Deliberately the *credential* and not the address: a provider's `base_url` is a
    /// chat-completions endpoint and search is not served there. See
    /// [`crate::search::DEEPSEEK_ANTHROPIC`] and `docs/deepseek-search.md` §1.
    #[serde(default)]
    pub provider: String,

    /// Override the search endpoint. Empty means DeepSeek's Anthropic-compatible one.
    #[serde(default)]
    pub base_url: String,

    /// Override the model the search runs on. Empty means [`crate::search::DEFAULT_MODEL`].
    ///
    /// The model is not the one driving the conversation: DeepSeek performs the search inside
    /// a model turn of its own, and this is which model that is.
    #[serde(default)]
    pub model: String,

    /// How many searches one call may trigger. Zero means the default.
    ///
    /// Not a hard limit -- measured: a call asking for one made two (`docs/deepseek-search.md`
    /// §3) -- but it is the only dial there is.
    #[serde(default)]
    pub max_uses: u32,

    /// A key written here, or the name of the variable holding it.
    ///
    /// Both are honoured in the same order a provider honours them: the variable first, then
    /// the literal. Used when `provider` names nothing.
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_key_env: String,
}

fn default_true() -> bool {
    true
}

/// Written out rather than derived, because a derived `Default` would give
/// `enabled: false` while the serde default for the same field is `true` -- so a block
/// deserialised from a file and a block built in code would disagree about whether search
/// is on. The two defaults have to be the same one.
impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            enabled: true,
            provider: String::new(),
            base_url: String::new(),
            model: String::new(),
            max_uses: 0,
            api_key: String::new(),
            api_key_env: String::new(),
        }
    }
}

impl SearchConfig {
    /// The key this block holds itself, ignoring any provider it might name.
    ///
    /// Resolved the way [`ProviderConfig::resolved_key`] resolves one, and for the same
    /// reason: the environment variable wins, so a key can be kept out of the file.
    pub fn own_key(&self) -> String {
        if !self.api_key_env.trim().is_empty() {
            if let Ok(value) = std::env::var(self.api_key_env.trim()) {
                if !value.trim().is_empty() {
                    return value;
                }
            }
        }
        self.api_key.clone()
    }
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

    /// Ceiling on the characters of conversation sent in one request. Past it, the oldest turns are
    /// left out of the *request* -- the session file keeps every one of them -- and a note says how
    /// many went. The system prompt and the newest turn are never dropped. 0 turns the guard off.
    #[serde(default = "default_max_request_chars")]
    pub max_request_chars: usize,

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

    /// How much of the agent's own activity to print: `"off"`, `"on"` (the default) or `"full"`.
    ///
    /// A word rather than the `bool` this used to be, and the reason is that a bool could not say
    /// it: `false` was both "off" and the default, so `/verbose off` wrote `false` and the next
    /// run came back *on* -- the quietest setting could be chosen and never kept. It also made
    /// `/config` print `verbose = false` while the run was printing a line per tool call.
    ///
    /// A bool in an existing file is still read, as what it has always meant: `false` is `"on"`
    /// and `true` is `"full"`. The next save writes the word, so the file stops being able to
    /// disagree with the run.
    #[serde(
        default,
        serialize_with = "ser_verbosity",
        deserialize_with = "de_verbosity"
    )]
    pub verbose: crate::display::Verbosity,

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

    /// Offer the tools one at a time, through the `tools` tool, instead of all at once.
    ///
    /// Every tool's description and argument schema is sent on **every request**, for the life of
    /// the tool: thirteen of them measured 10,710 characters, which was four times the system
    /// prompt. This keeps one small tool in the request and a one-line catalogue of the rest, and
    /// a tool joins the request when the model asks for it -- so a run pays for what it uses
    /// rather than for everything that exists. Measured on a fifteen-turn run that used four
    /// tools: about 55,500 characters of tool payload against 160,650.
    ///
    /// The cost is one extra turn per tool the first time it is needed: the model asks what the
    /// arguments are, and calls it from the next turn on. `lazy_tools = false` sends everything
    /// every time, which is what every version before this did.
    #[serde(default = "default_true")]
    pub lazy_tools: bool,

    /// The tools declared on every request even when `lazy_tools` is on.
    ///
    /// Absent means the eight whose arguments follow a convention a model already knows (`read`,
    /// `write`, `edit`, `list`, `glob`, `grep`, `bash`, `exec`) -- the ones a run reaches for
    /// constantly, which must not depend on the model choosing to look something up. A list replaces
    /// that, and an **empty** list is the all-lazy shape: nothing but the lookup, for anybody who
    /// wants to measure whether their model asks before it guesses.
    #[serde(default)]
    pub eager_tools: Option<Vec<String>>,

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

    /// Web search, when it is configured at all. See [`SearchConfig`].
    #[serde(default)]
    pub search: Option<SearchConfig>,

    /// How much reasoning to ask the provider for: `"off"`, `"low"`, `"medium"` or `"high"`.
    ///
    /// A word, because the ladder is a ladder and a bool cannot hold four rungs. `"off"` is the
    /// default and means *ask for nothing*, which is what flint has always done -- and it is not the
    /// same as telling an endpoint to reason less: flint does not know that vendor's word for it, and
    /// silence is honest where a guessed word is a refusal in the middle of a turn.
    ///
    /// The *field* the level goes in is the provider's (`thinking_field`), because that is the part
    /// vendors disagree about. Both have to be set for anything to be sent.
    ///
    /// A conversation remembers the level it was held at (a `thinking` line in its file), and
    /// `--thinking` or `/thinking` overrides that for the run in front of you.
    #[serde(default = "default_thinking")]
    pub thinking: String,

    pub providers: Vec<ProviderConfig>,
}

/// The reasoning level flint asks for without being told anything: nothing at all.
fn default_thinking() -> String {
    "off".to_string()
}

fn default_instructions() -> String {
    crate::context::Instructions::DEFAULT_NAME.to_string()
}

fn default_max_tool_output() -> usize {
    30_000
}

/// Characters of *conversation* a request may carry, in the same unit as `max_tool_output`.
///
/// A guard in the spirit of `max_steps`, and for the same reason one is needed: without it a long
/// conversation grows the request until the provider refuses it, and that arrives as an error in the
/// middle of a turn rather than as a decision anybody made. What is dropped when the budget is
/// reached is the oldest *turns*, from the request only -- the session file keeps every message, and
/// a note in their place says how many went and where they are. The system prompt and the newest
/// turn are never dropped, so a request can be a little over this number.
///
/// A character count rather than a token count because flint has no tokenizer and will not grow one
/// for this: 400,000 characters is a little over 100,000 tokens of English prose and code, which
/// leaves room under a modern context window for the answer. A model with a small window wants this
/// set lower; a model with a huge one, or a person who would rather see the provider's error than a
/// dropped turn, sets it to 0, which turns the guard off.
fn default_max_request_chars() -> usize {
    400_000
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

/// `verbose` as its word: `verbose = "on"`.
fn ser_verbosity<S: serde::Serializer>(
    value: &crate::display::Verbosity,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(value.word())
}

/// The word, or the bool the key used to hold.
///
/// The bool is not a kindness to old files so much as a statement of what it meant: `false` was
/// the default and printed one line per tool call, which is `"on"` and not `"off"`. Reading it as
/// `"off"` would turn every existing config into a silent one.
fn de_verbosity<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<crate::display::Verbosity, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Written {
        Word(String),
        Flag(bool),
    }

    match Written::deserialize(deserializer)? {
        Written::Word(word) => crate::display::Verbosity::from_word(word.trim()).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "expected one of off, on, full for `verbose`, got {word:?}"
            ))
        }),
        Written::Flag(true) => Ok(crate::display::Verbosity::Full),
        Written::Flag(false) => Ok(crate::display::Verbosity::On),
    }
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

    /// The root the API hangs off, whatever shape `base_url` was written in.
    ///
    /// `endpoint()` appends `/chat/completions` to this, and a preflight appends its own path, so the
    /// stripping has to live in one place: a `base_url` that already names the chat endpoint would
    /// otherwise ask for `/chat/completions/user/balance`, and the answer would be a 404 that reads
    /// like a provider with no balance API.
    pub fn api_root(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        let root = base.strip_suffix("/chat/completions").unwrap_or(base);
        root.trim_end_matches('/').to_string()
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_provider: "deepseek".to_string(),
            shell: default_shell(),
            shell_args: default_shell_args(),
            max_tool_output: default_max_tool_output(),
            max_request_chars: default_max_request_chars(),
            max_steps: default_max_steps(),
            readonly: false,
            proxy: None,
            verbose: crate::display::Verbosity::On,
            tool_detail: false,
            lazy_tools: true,
            eager_tools: None,
            instructions: default_instructions(),
            skill_dirs: Vec::new(),
            // Absent on purpose: with no block, search inherits the credential of the
            // DeepSeek provider below, which is what "configured once" should mean.
            search: None,
            thinking: default_thinking(),
            providers: vec![
                ProviderConfig {
                    name: "deepseek".to_string(),
                    base_url: "https://api.deepseek.com/v1".to_string(),
                    api_key: String::new(),
                    model: "deepseek-chat".to_string(),
                    models: vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()],
                    api_key_env: Some("DEEPSEEK_API_KEY".to_string()),
                    start: None,
                    stop: None,
                    start_timeout_secs: 0,
                    proxy: None,
                    // DeepSeek takes `reasoning_effort`, and writing it into the default provider is
                    // what makes `--thinking high` work out of the box for the endpoint flint ships
                    // configured. Nothing is sent until a level is asked for, so this is not a change
                    // for anyone who has not: the two halves are deliberately independent.
                    thinking_field: "reasoning_effort".to_string(),
                },
                // A local fallback costs nothing to configure and still works
                // when every hosted provider is unreachable.
                ProviderConfig {
                    name: "ollama".to_string(),
                    base_url: "http://localhost:11434/v1".to_string(),
                    api_key: "ollama".to_string(),
                    model: "qwen2.5-coder:7b".to_string(),
                    models: Vec::new(),
                    start: None,
                    stop: None,
                    start_timeout_secs: 0,
                    api_key_env: None,
                    proxy: None,
                    // A local model server is nobody's reasoning endpoint: `ollama` has its own way
                    // of asking (a `think` field on newer builds) and flint will not guess at it. The
                    // cost of being wrong here is a refused request, so silence is the default.
                    thinking_field: String::new(),
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

/// The directory this user's `~` means, or nothing when the machine will not say.
///
/// The two shapes exist because the two callers want different things from an unknown home: a
/// caller *choosing where to write* falls back to the working directory, and a caller *resolving a
/// path somebody wrote with a `~` in it* must not -- `~/notes.txt` silently becoming `./notes.txt`
/// is a file read out of a directory nobody named, which is worse than saying the path is not there.
/// One resolver, so `FLINT_HOME`'s default and a `~` in a tool argument cannot name two directories.
pub fn home_dir_or_none() -> Option<PathBuf> {
    dirs::home_dir()
}

pub fn home_dir() -> PathBuf {
    home_dir_or_none().unwrap_or_else(|| PathBuf::from("."))
}

/// A path written with a leading `~`, as the path it means.
///
/// The rule is the shell's, narrowed to the one case this program can answer: `~` followed by a
/// separator is this user's home directory. Three things are deliberately *not* expanded, because
/// each would be a guess:
///
/// - `~user/notes.txt` -- another user's home. `dirs` knows the home of whoever is running, and
///   nothing here can say where somebody else's is; a guess would be a read of the wrong file.
/// - `~notes.txt` -- not a home at all. On Unix that is a file whose *name* begins with a tilde,
///   and a program that quietly turned it into a path in the home directory would be unable to
///   open the file it was pointed at.
/// - `~/` with no home to expand to (a machine that will not say): the path stays as written, and
///   the reader that asked reports it as missing.
///
/// `a/~/b` is not expanded either: the tilde has to be the first character, which is what makes
/// this a rule about the *beginning* of a path rather than a search for a character in it.
///
/// The page's own address scanner states the same rule in `asPath` (`web/view.html`) so that a
/// `~/…` in a transcript is a button and `~notes.txt` is a word: two readers, one definition of
/// what a tilde path is.
pub fn expand_home(path: &str) -> PathBuf {
    expand_home_in(path, home_dir_or_none().as_deref())
}

/// The pure half of [`expand_home`], with the home handed in -- so the rule can be tested for the
/// cases a real machine cannot be asked about (no home at all, and a path that is not ours to guess).
pub fn expand_home_in(path: &str, home: Option<&std::path::Path>) -> PathBuf {
    if let (Some(rest), Some(home)) = (tilde_rest(path), home) {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// What follows a `~` that means this user's home, or nothing when the tilde is a character in a
/// name. The separator is required: see [`expand_home`] for the two shapes that are not a home.
fn tilde_rest(path: &str) -> Option<&str> {
    let rest = path.strip_prefix('~')?;
    rest.strip_prefix(['/', '\\'])
}

/// A path somebody wrote, as the path it means: a leading `~` is this user's home directory (see
/// [`expand_home`]), an absolute path is taken as written, and anything else is relative to `cwd`.
///
/// One function for the four doors a path comes in through -- a model's tool argument, the page's
/// two routes (`GET /file`, `POST /open`), a person's `@name`, and a directory named in
/// `config.toml` -- because four copies of "absolute, else against the working directory" is how a
/// `~` came to be expanded by none of them, and how they could come to disagree about the rest.
pub fn resolve_path(cwd: &Path, raw: &str) -> PathBuf {
    let path = expand_home(raw);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
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

/// Where a running flint says it is alive, so that another one can see it.
///
/// Under `FLINT_HOME` rather than in the project, for one reason: a `readonly` run must still be able
/// to announce itself, and a checkout is not always writable. That is why this stays the home every
/// run writes to, even now that a project can carry a copy: the home record is the one that cannot
/// fail. [`project_dir`] is the copy, and it exists to be *shared* rather than to be reliable -- two
/// installations pointed at different `FLINT_HOME`s see each other through it and through nothing else.
pub fn live_dir() -> PathBuf {
    config_dir().join("live")
}

/// The `.flint/` directory of the project `cwd` belongs to, when that project has one.
///
/// **Its existence is the whole opt-in, and flint never creates it.** That is the price
/// `docs/agents.md` recorded for closing the two-`FLINT_HOME` hole -- writing into somebody's
/// checkout -- paid only where somebody has already paid it, with a skill, a profile, or a bare
/// `mkdir .flint`. A checkout that never asked for flint's project state stays untouched, which is
/// what makes this safe to do on every run.
///
/// Two directories are refused even when they are named `.flint`. The walk stops *at* the home
/// directory, because `~/.flint` is `FLINT_HOME`'s default and belongs to every project and none:
/// without that rule, every run under a home directory would see the marker, and `flint say` would
/// write one mailbox for the whole machine. And a `.flint` that *is* the configured `FLINT_HOME` is
/// skipped for the same reason, which covers a home pointed somewhere unusual.
pub fn project_dir(cwd: &std::path::Path) -> Option<PathBuf> {
    project_dir_until(cwd, &home_dir())
}

/// The walk above, with its stopping point passed in so that a test can put one somewhere it can
/// create -- a test cannot move the real home directory.
///
/// Both comparisons are by *place* rather than by string, and that is not tidiness: `%TEMP%` on a
/// Windows CI runner is the short name (`C:\Users\RUNNER~1\…`) while the home directory is the long
/// one, so `~/.flint` and a candidate built from the same directory did not compare equal -- and a
/// run's own home, created by the first `flint say`, then looked like a project marker. That is a
/// path that changes partway through a process, which is how a message gets written to one mailbox
/// and read from another; `a_stopping_point_is_recognised_however_it_is_spelled` is the test.
fn project_dir_until(cwd: &std::path::Path, stop: &std::path::Path) -> Option<PathBuf> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        if same_place(d, stop) {
            return None;
        }
        let candidate = d.join(".flint");
        if !same_place(&candidate, &config_dir()) && candidate.is_dir() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

/// Whether two paths name the same directory.
///
/// Compared as canonical paths, for the reason `session::same_dir` records about a session's
/// recorded `cwd`: one directory arrives spelled differently depending on who asked, and Windows
/// distinguishes neither case nor separator nor the 8.3 short name. When either side cannot be
/// canonicalised -- the normal state of a config directory that has never been created -- the paths
/// are compared as written with case and separators folded on Windows, because those never
/// distinguish two places there, and compared exactly elsewhere, because on Unix they do.
fn same_place(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => {
            #[cfg(windows)]
            {
                let fold = |p: &std::path::Path| {
                    p.to_string_lossy().replace('/', "\\").to_lowercase()
                };
                fold(a) == fold(b)
            }
            #[cfg(not(windows))]
            {
                a == b
            }
        }
    }
}

/// Where one directory's peers leave each other messages.
///
/// Under `FLINT_HOME` for the same reason the presence records are: a `readonly` run must be able to
/// be spoken to, and a project directory is not always writable. The cost is the same and is said in
/// the same place: two installations with different homes cannot talk to each other.
pub fn mailbox_dir() -> PathBuf {
    config_dir().join("mailbox")
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

/// What every generated `config.toml` says before its settings.
///
/// A constant rather than a literal inside [`Config::save`] so that a test can assert it
/// still explains the fields whose names do not explain themselves. This is where a person —
/// or an agent asked to configure flint — actually looks, and knowledge that lives only in
/// `engine.rs` is knowledge nobody configuring flint has.
pub const CONFIG_HEADER: &str = "\
# flint configuration.
# `base_url` must be OpenAI-compatible: POST {base_url}/chat/completions
# Add as many providers as you like and switch with --provider <name> or /provider.
#
# A local model server is started and stopped for you as you switch between providers:
#
#   start = \"...\"    run when flint needs this provider and nothing answers at base_url
#   stop  = \"...\"    run when a switch leaves this provider behind
#   start = \"\"       this one is not flint's to manage -- leave it alone
#
# Leaving `start` out asks flint to use what it knows, and it knows three engines by name: a
# provider called `ollama`, `mlx` or `llamacpp` has its start command derived from the name.
# That only happens when it can work -- the endpoint is on this machine, the program is on
# PATH, and for `mlx` and `llamacpp` the `model` field names what to load. When it cannot,
# flint says what is missing rather than running a command that would fail. Setting `start`
# yourself always wins.
#
# Engine output, and the reason a start failed, is in <FLINT_HOME>/engines/<provider>.log.

";

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
            // Two notices rather than one message with a newline in it: one `line` call carrying a
            // newline moves the cursor down through rows the layout reserved for something else.
            // This is reachable mid-session -- `/reload` with the file deleted -- which is why it
            // goes through the sink instead of straight to stderr.
            crate::tools::notice(&format!(
                "created default config at {}",
                path.display()
            ));
            crate::tools::notice(
                "set your API key there (or export DEEPSEEK_API_KEY), then re-run.",
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
        // Checked here rather than where the request is built, so a typo is a startup complaint
        // naming the file and the words it takes, and not a turn that quietly asked for nothing.
        if !crate::provider::Thinking::is_a_level(&self.thinking) {
            return Err(anyhow::anyhow!(
                "unknown thinking level '{}' in {}.\n\
                 Use one of {}, or drop the key: \"off\" is the default and asks the provider \
                 for no reasoning parameter at all.",
                self.thinking,
                config_path().display(),
                crate::provider::Thinking::LEVELS.join(", ")
            ));
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(config_dir()).context("cannot create config directory")?;
        let body = toml::to_string_pretty(self).context("cannot serialize config")?;
        let header = CONFIG_HEADER;

        let path = config_path();
        std::fs::write(&path, format!("{header}{body}"))
            .with_context(|| format!("cannot write config {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `.flint/` walk: the nearest marker wins, and the stopping point is never inspected.
    ///
    /// The second half is the one worth a test, because it is invisible in the happy case and
    /// catastrophic in the unhappy one: `~/.flint` is `FLINT_HOME`'s default, so a walk that looked
    /// *at* the home directory would find a marker above every project on the machine, and every
    /// run under a home directory would write its presence into the same file -- and `flint say`
    /// would leave one mailbox for the whole machine instead of one per project.
    #[test]
    fn the_project_marker_is_the_nearest_one_and_never_the_home_directory() {
        let root = std::env::temp_dir().join(format!("flint-project-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let nested = root.join("project/src/deep");
        std::fs::create_dir_all(&nested).expect("directories");
        let far = root.join("somewhere-else");
        assert_eq!(
            project_dir_until(&nested, &far),
            None,
            "a tree with no marker anywhere has none"
        );

        // The home directory's own `.flint`, which is FLINT_HOME and not a project's marker.
        std::fs::create_dir_all(root.join(".flint")).expect("home marker");
        assert_eq!(
            project_dir_until(&nested, &root),
            None,
            "the walk looked at the directory it is supposed to stop before"
        );

        // ...and a marker *below* that stopping point is found, and is the nearest one.
        std::fs::create_dir_all(root.join("project/.flint")).expect("project marker");
        assert_eq!(
            project_dir_until(&nested, &root),
            Some(root.join("project/.flint"))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The walk, and the two directories it refuses, whatever they are spelled like.
    ///
    /// This is the defect the Windows CI found. On a GitHub runner `%TEMP%` is the *short* name
    /// (`C:\Users\RUNNER~1\AppData\Local\Temp`) while the home directory is the long one, so the
    /// home's `~/.flint` did not compare equal to the candidate the walk had just built out of the
    /// same directory -- and the moment a run created its own home (which is the first thing `flint
    /// say` does), the home's `FLINT_HOME` started looking like a project marker. A path that
    /// changes partway through a process is how one mailbox gets written and another read, which is
    /// the failure the run after this one reported. Case is the same shape of difference on this
    /// platform and the one below holds it: it never distinguishes two places, so a walk that
    /// compares strings gets it wrong the same way.
    #[cfg(windows)]
    #[test]
    fn a_stopping_point_is_recognised_however_it_is_spelled() {
        let root =
            std::env::temp_dir().join(format!("flint-project-spelling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let nested = home.join("work/deep");
        std::fs::create_dir_all(&nested).expect("directories");
        // The home's own `.flint`, which is `FLINT_HOME` and never a project's marker.
        std::fs::create_dir_all(home.join(".flint")).expect("home marker");

        // The same tree, spelled the way a short name or a differently cased path arrives.
        let spelled = std::path::PathBuf::from(root.to_string_lossy().to_uppercase());
        assert_eq!(
            project_dir_until(&spelled.join("home/work/deep"), &home),
            None,
            "a differently spelled home directory was not recognised as the stopping point, so its \
             own .flint was taken for a project's"
        );

        // ...and a marker below the stopping point is still found, so this is not "refuse everything".
        std::fs::create_dir_all(home.join("work/.flint")).expect("project marker");
        assert_eq!(
            project_dir_until(&spelled.join("home/work/deep"), &home),
            Some(spelled.join("home/work/.flint")),
            "the marker below the stopping point was not found"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The same fact on the platform without 8.3 names and without case folding: one directory,
    /// two names, and the walk has to recognise the stopping point either way.
    #[cfg(unix)]
    #[test]
    fn a_stopping_point_is_recognised_however_it_is_spelled() {
        let root = std::env::temp_dir().join(format!("flint-project-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let nested = home.join("work/deep");
        std::fs::create_dir_all(&nested).expect("directories");
        std::fs::create_dir_all(home.join(".flint")).expect("home marker");
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&home, &alias).expect("symlink");

        assert_eq!(
            project_dir_until(&alias.join("work/deep"), &home),
            None,
            "a home directory reached through another name was not recognised as the stopping \
             point, so its own .flint was taken for a project's"
        );

        std::fs::create_dir_all(home.join("work/.flint")).expect("project marker");
        assert_eq!(
            project_dir_until(&alias.join("work/deep"), &home),
            Some(alias.join("work/.flint")),
            "the marker below the stopping point was not found"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The generated file has to explain the engine fields, because it is where a person —
    /// or an agent asked to configure flint — actually looks. Knowledge that lives only in
    /// `engine.rs` is knowledge nobody configuring flint has.
    #[test]
    fn the_generated_config_explains_the_engine_fields() {
        for needed in [
            "start", "stop", "ollama", "mlx", "llamacpp", "PATH", "engines/",
        ] {
            assert!(
                CONFIG_HEADER.contains(needed),
                "the generated config never mentions {needed:?}, so a reader will not find it"
            );
        }
        // This is *why* the header has to carry it. `start` and `stop` are
        // `Option<String>`, and TOML cannot write a null — so an unset one is not in the
        // file at all, and what a generated config shows is `start_timeout_secs = 0` with no
        // `start` to go with it. That dangling key is the hook; the header is the
        // explanation. `/provider` is the third, and the one at the point of use: it says
        // what flint would run for each engine (`engine::describe`).
        let body = toml::to_string_pretty(&Config::default()).expect("serialize");
        assert!(
            body.contains("start_timeout_secs"),
            "the one engine key that is written is the hook a reader notices: {body}"
        );
        assert!(
            !body.contains("\nstart = "),
            "if `start` ever starts serialising, the header stops being the only place it \
             is explained, and this test should be rewritten rather than deleted: {body}"
        );
    }

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

    /// A reasoning level is refused by name, with the words that would have worked.
    ///
    /// The same shape as the instruction-mode test above and for the same reason: this value reaches
    /// a *request*, so a level this build does not know is either a 400 in the middle of a turn or --
    /// worse, because it is silent -- a word the endpoint ignores, leaving a run that looks like it
    /// is reasoning hard and is not.
    #[test]
    fn an_unknown_thinking_level_is_rejected_by_name() {
        let cfg = Config {
            thinking: "loud".to_string(),
            ..Config::default()
        };
        let err = format!("{:#}", cfg.validate().unwrap_err());
        assert!(err.contains("loud"), "{err}");
        for level in crate::provider::Thinking::LEVELS {
            assert!(err.contains(level), "the error must offer {level}: {err}");
        }
    }

    /// A `~` at the start of a path is the home directory -- and nothing else is.
    ///
    /// The cases these assert are the ones where a rule like this goes wrong rather than the happy
    /// one: `~user` is somebody else's home, `~notes.txt` is a file *named* that, `a/~/b` has a tilde
    /// that is not at the start, and a machine that will not say where home is must leave the path
    /// alone rather than resolve it against whatever the process happens to be sitting in.
    #[test]
    fn a_leading_tilde_is_the_home_directory_and_nothing_else_is() {
        let home = Path::new("/home/me");
        assert_eq!(expand_home_in("~/notes.txt", Some(home)), PathBuf::from("/home/me/notes.txt"));
        assert_eq!(expand_home_in("~\\notes.txt", Some(home)), PathBuf::from("/home/me/notes.txt"));
        assert_eq!(expand_home_in("~/", Some(home)), PathBuf::from("/home/me/"));
        // The tilde is the *first* character or it is a character in a name.
        assert_eq!(expand_home_in("a/~/b", Some(home)), PathBuf::from("a/~/b"));
        assert_eq!(expand_home_in("~notes.txt", Some(home)), PathBuf::from("~notes.txt"));
        assert_eq!(expand_home_in("~", Some(home)), PathBuf::from("~"));
        // `~user` is another user's home, which nothing here can know: left as written, so the
        // reader that asked reports it missing instead of reading a file that is not the one named.
        assert_eq!(expand_home_in("~user/x", Some(home)), PathBuf::from("~user/x"));
        // No home to expand to: the path stays what somebody wrote.
        assert_eq!(expand_home_in("~/x", None), PathBuf::from("~/x"));
        // Ordinary paths are not touched at all.
        assert_eq!(expand_home_in("C:\\x\\y.md", Some(home)), PathBuf::from("C:\\x\\y.md"));
        assert_eq!(expand_home_in("src/main.rs", Some(home)), PathBuf::from("src/main.rs"));
    }

    /// The one resolution rule, with the home handed in so the assertion does not depend on whose
    /// machine this runs on: `~` first, then absolute, then against the working directory.
    #[test]
    fn a_path_resolves_through_the_home_directory() {
        // `resolve_path` reads the real home, so the *rule* is asserted through the pure half and
        // this test is about the shape of it: the result is absolute for every input.
        let cwd = std::env::temp_dir();
        assert!(resolve_path(&cwd, "a/b").is_absolute(), "relative to the working directory");
        // "Absolute" is the platform's own notion of it -- `Path::is_absolute`, which on Windows
        // wants a drive in front, so the two spellings are asserted one per platform rather than
        // one of them being asked to mean the same thing on both.
        let absolute = if cfg!(windows) { "C:\\etc\\hosts" } else { "/etc/hosts" };
        assert_eq!(resolve_path(&cwd, absolute), PathBuf::from(absolute), "absolute as written");
        let home = home_dir();
        assert_eq!(resolve_path(&cwd, "~/x"), home.join("x"), "and a tilde in the real home");
        assert_eq!(resolve_path(&cwd, "~x"), cwd.join("~x"), "which is not a home path");
    }
}
