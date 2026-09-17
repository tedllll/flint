//! The things a project has written down, and the model cannot guess.
//!
//! Two of them, and they are the same idea twice: a file the user already maintains and
//! that flint should not silently ignore.
//!
//! - **Instruction files** (`AGENTS.md`) describe how a project wants to be worked on.
//!   They are *named* by default rather than pasted: the model reads what it needs with
//!   the `read` tool, and a file someone edits mid-session is picked up immediately
//!   instead of being frozen into a prompt built at start-up. `paste` is available for
//!   anyone who wants them in every request, and `off` for anyone who wants neither.
//! - **Skills** (`<dir>/<name>/SKILL.md`) are procedures worth following step by step.
//!   The prompt gets a catalog of one-line summaries; the body arrives only when the
//!   model asks for it by name, so a skill costs a line until it is used.
//!
//! Nothing here is cached and nothing is derived. Every lookup goes to the filesystem,
//! which is what makes a skill added while flint is running work on the next call.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// How much of the instruction files may enter the prompt in `paste` mode.
///
/// The same ceiling Codex uses. A prompt that swallows a repository's whole
/// documentation is worse than one that names the file -- which is exactly why `hint`
/// is the default and this is a limit rather than a target.
pub const PASTE_BUDGET: usize = 32 * 1024;

/// Longest catalog line, and the most catalog there may be.
///
/// A summary is for recognising a skill, not for describing it: the model either
/// recognises the name or loads the body, and anything longer spends context on every
/// request to save a call that costs one.
const DESCRIPTION_LIMIT: usize = 100;
const CATALOG_LIMIT: usize = 1200;

/// What to do with instruction files. Parsed from the config's `instructions` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instructions {
    /// Say nothing. The model may still find the files itself.
    Off,
    /// Name them, and let the model read them.
    Hint,
    /// Include their contents in the system prompt.
    Paste,
}

impl Instructions {
    /// The value written into a new config, and the fallback for an unreadable one.
    pub const DEFAULT_NAME: &'static str = "hint";

    /// Parse a config value. `None` means the value is not one of the three, and the
    /// caller should say so rather than quietly behaving like the default.
    pub fn parse(raw: &str) -> Option<Instructions> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" => Some(Instructions::Off),
            "hint" => Some(Instructions::Hint),
            "paste" | "full" | "true" => Some(Instructions::Paste),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Instructions::Off => "off",
            Instructions::Hint => "hint",
            Instructions::Paste => "paste",
        }
    }
}

/// One skill: a named directory holding `SKILL.md`.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// One agent profile: a named way to start another run.
///
/// A profile is not a new kind of thing. It is the arguments `flint -p … --json` already takes --
/// a model, a provider, `readonly`, and the words to start from -- written down once, in a file, so
/// that "the explorer" means the same thing to a person typing it and to a model naming it. That is
/// the whole feature: the alternative is a model composing a command line, which is where a wrong
/// flag becomes somebody's bill.
///
/// The body is deliberately *not* carried here. Discovery reads only the front matter, the way the
/// skill catalog does, so a directory of long profiles costs a few lines of prompt rather than all
/// of their text; the body is read when the profile is used.
#[derive(Debug, Clone)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    /// A model for the child. `None` means "whatever this run is using".
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Whether the child may write. A profile can *add* `readonly`, never remove it: the run that
    /// spawns decides the floor, the same way the `task` tool does.
    pub readonly: bool,
    pub path: PathBuf,
}

impl AgentProfile {
    /// One line for a catalog: the name, what it is for, and the two facts that change behaviour.
    pub fn summary(&self) -> String {
        let mut out = self.name.clone();
        if !self.description.is_empty() {
            out.push_str(" — ");
            out.push_str(&self.description);
        }
        let mut facts: Vec<String> = Vec::new();
        if self.readonly {
            facts.push("readonly".to_string());
        }
        if let Some(model) = &self.model {
            facts.push(format!("model {model}"));
        }
        if let Some(provider) = &self.provider {
            facts.push(format!("provider {provider}"));
        }
        if !facts.is_empty() {
            out.push_str(&format!(" [{}]", facts.join(", ")));
        }
        out
    }
}

/// What was found around a working directory.
#[derive(Debug, Clone, Default)]
pub struct Workspace {
    /// The enclosing project: the nearest directory holding `.git`.
    pub root: Option<PathBuf>,
    /// `AGENTS.md` files, outermost first, so the closest to the work is read last.
    pub instruction_files: Vec<PathBuf>,
    pub skills: Vec<Skill>,
    /// The directories those skills were found in, in priority order. Carried so a
    /// caller that wants to load one -- the `skill` tool, `/skills` -- searches exactly
    /// what the prompt's catalog was built from.
    pub skill_dirs: Vec<PathBuf>,
    /// Agent profiles, by name, nearest directory first.
    pub agents: Vec<AgentProfile>,
    /// The directories those profiles were found in, for the same reason as `skill_dirs`.
    pub agent_dirs: Vec<PathBuf>,
    /// Prompt templates, by name, nearest directory first. The person's door, not the model's: see
    /// `Prompt`.
    pub prompts: Vec<Prompt>,
    /// The directories those templates were found in, for the same reason as `skill_dirs`.
    pub prompt_dirs: Vec<PathBuf>,
}

impl Workspace {
    /// Look around `cwd`, using the machine's own config and home directories.
    pub fn discover(cwd: &Path, extra_skill_dirs: &[String]) -> Workspace {
        discover_with(
            cwd,
            &crate::config::config_dir(),
            &crate::config::home_dir(),
            extra_skill_dirs,
        )
    }

    /// Load one skill's body by name, re-read from disk.
    ///
    /// Through the same function the `skill` tool uses, so what `/skills <name>` prints
    /// is what the model would receive.
    pub fn load(&self, name: &str) -> Result<String> {
        load_skill(&self.skill_dirs, name)
    }

    /// Load one agent profile's body by name, re-read from disk.
    ///
    /// The profile itself comes from what was already discovered -- a name is looked up in a list,
    /// never turned into a path -- and only the body is read again, so that editing a profile's
    /// instructions takes effect in the next child without restarting the run.
    pub fn load_agent(&self, name: &str) -> Result<(AgentProfile, String)> {
        let profile = self.agent(name)?;
        let body = load_agent_body(&self.agent_dirs, name)?;
        Ok((profile, body))
    }

    /// One profile by name, or an error listing the ones that exist.
    pub fn agent(&self, name: &str) -> Result<AgentProfile> {
        self.agents
            .iter()
            .find(|agent| agent.name == name)
            .cloned()
            .ok_or_else(|| unknown_agent_error(name, &self.agents))
    }

    /// One skill by name, or nothing.
    ///
    /// The listing's counterpart to `prompt`, and the same reason for the `Option`: a caller that
    /// wants the file a body came from should not have to load the body to find out.
    pub fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    /// One template by name, or nothing.
    ///
    /// `Option` rather than an error, because the caller is the dispatcher's last resort: a word
    /// that is neither a command nor a template is an unknown command, and that message -- with
    /// `/help` and `/prompts` named in it -- is a better answer than naming a template list to
    /// somebody who was not asking about templates.
    pub fn prompt(&self, name: &str) -> Option<&Prompt> {
        self.prompts.iter().find(|prompt| prompt.name == name)
    }

    /// Load one template's body by name, re-read from disk.
    ///
    /// Re-read rather than remembered, like `/reload`'s catalog: a saved prompt is exactly the kind of
    /// file a person tweaks between two uses of it, and the next `/name` should send what is on disk.
    pub fn load_prompt(&self, name: &str) -> Result<String> {
        load_prompt(&self.prompt_dirs, name)
    }

    /// The skill names, in the order the model is given them.
    ///
    /// Names only, and deliberately: the description is prose for the *model*, and a caller that
    /// wants it in front of a person should ask `/skills` for the listing rather than re-render
    /// the catalog itself. What this answers is "which of these may be named", which is what a
    /// menu needs.
    pub fn skill_names(&self) -> Vec<String> {
        self.skills.iter().map(|skill| skill.name.clone()).collect()
    }

    /// The template names, for the same reader as `skill_names`: the page's menu.
    pub fn prompt_names(&self) -> Vec<String> {
        self.prompts
            .iter()
            .map(|prompt| prompt.name.clone())
            .collect()
    }

    /// The text to append to the system prompt. Empty when there is nothing to say.
    pub fn prompt_note(&self, mode: Instructions) -> String {
        let mut out = String::new();

        if mode != Instructions::Off && !self.instruction_files.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            match mode {
                Instructions::Paste => out.push_str(&self.pasted_instructions()),
                _ => {
                    out.push_str(
                        "Project instruction files exist. Read them before changing \
                         anything, and do not assume their contents:\n",
                    );
                    for path in &self.instruction_files {
                        out.push_str(&format!("- {}\n", path.display()));
                    }
                }
            }
        }

        if !self.skills.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(
                "Skills available. Call the `skill` tool with the name to load one, \
                 and follow it once loaded:\n",
            );
            let mut used = 0usize;
            let mut listed = 0usize;
            for skill in &self.skills {
                let line = format!(
                    "- {}: {}\n",
                    skill.name,
                    crate::util::clip(&skill.description, DESCRIPTION_LIMIT)
                );
                if used + line.len() > CATALOG_LIMIT {
                    break;
                }
                used += line.len();
                listed += 1;
                out.push_str(&line);
            }
            if listed < self.skills.len() {
                out.push_str(&format!(
                    "({} more not listed here; `/skills` shows all of them.)\n",
                    self.skills.len() - listed
                ));
            }
            out.push_str(
                "This catalog is summaries only: do not infer or follow a skill's \
                 instructions until it has been loaded.",
            );
        }

        out
    }

    /// The instruction files, with contents, inside the budget.
    fn pasted_instructions(&self) -> String {
        let mut out = String::from(
            "Project instruction files, in order from most general to most specific. \
             Follow them:\n",
        );
        let mut remaining = PASTE_BUDGET;
        let mut skipped: Vec<&PathBuf> = Vec::new();
        for path in &self.instruction_files {
            if remaining == 0 {
                skipped.push(path);
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                skipped.push(path);
                continue;
            };
            let header = format!("\n--- {} ---\n", path.display());
            if text.len() + header.len() > remaining {
                // Cut on a character boundary: a prompt is not a place to emit half a
                // UTF-8 sequence, and the model would read the damage as content.
                let mut keep = remaining.saturating_sub(header.len());
                while keep > 0 && !text.is_char_boundary(keep) {
                    keep -= 1;
                }
                out.push_str(&header);
                out.push_str(&text[..keep]);
                out.push_str(&format!(
                    "\n[... truncated at {PASTE_BUDGET} bytes; read {} with the read \
                     tool for the rest]\n",
                    path.display()
                ));
                remaining = 0;
                continue;
            }
            remaining -= text.len() + header.len();
            out.push_str(&header);
            out.push_str(&text);
            if !text.ends_with('\n') {
                out.push('\n');
            }
        }
        if !skipped.is_empty() {
            out.push_str("\nNot included (over the pasted budget); read them if relevant:\n");
            for path in skipped {
                out.push_str(&format!("- {}\n", path.display()));
            }
        }
        out
    }
}

/// `Workspace::discover`, with the two ambient directories passed in.
///
/// Split out so a test can point at a temporary tree instead of the machine's real home
/// directory -- and so the discovery rule is asserted on its own rather than through
/// whatever happens to exist on the machine running the test.
pub fn discover_with(
    cwd: &Path,
    config_dir: &Path,
    home: &Path,
    extra_skill_dirs: &[String],
) -> Workspace {
    let mut instruction_files: Vec<PathBuf> = Vec::new();

    // The user's own file applies everywhere, so it comes first and is the most general.
    let user_instructions = config_dir.join("AGENTS.md");
    if user_instructions.is_file() {
        instruction_files.push(user_instructions);
    }

    // From the working directory upwards. `nearest` is closest-first; the prompt wants
    // the general one before the specific one, so it is reversed below.
    let mut nearest: Vec<PathBuf> = Vec::new();
    let mut root: Option<PathBuf> = None;
    let mut cursor = Some(cwd.to_path_buf());
    while let Some(dir) = cursor {
        nearest.push(dir.clone());
        if dir.join(".git").exists() {
            root = Some(dir);
            break;
        }
        // A file above the user's home directory belongs to nobody in particular, and
        // walking to the drive root to find one is how a stray `AGENTS.md` in `C:\`
        // ends up steering every conversation.
        if dir == home {
            break;
        }
        cursor = dir.parent().map(Path::to_path_buf);
    }
    for dir in nearest.iter().rev() {
        let file = dir.join("AGENTS.md");
        if file.is_file() {
            instruction_files.push(file);
        }
    }

    let dirs = skill_dirs_for(cwd, config_dir, root.as_deref(), extra_skill_dirs);
    let agent_dirs = agent_dirs_for(cwd, config_dir, root.as_deref());
    let prompt_dirs = prompt_dirs_for(cwd, config_dir, root.as_deref());
    Workspace {
        root,
        instruction_files,
        skills: skills_in(&dirs),
        skill_dirs: dirs,
        agents: agents_in(&agent_dirs),
        agent_dirs,
        prompts: prompts_in(&prompt_dirs),
        prompt_dirs,
    }
}

/// Which directories are searched for skills, in the order that decides name conflicts.
///
/// The project's own skills win, then the working directory's, then the user's, then
/// anything named in the config. A project that ships a `tidy-commits` skill means it,
/// and the user's copy of the same name is the fallback rather than the override.
pub fn skill_dirs_for(
    cwd: &Path,
    config_dir: &Path,
    root: Option<&Path>,
    extra: &[String],
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    if let Some(root) = root {
        push(root.join(".flint").join("skills"));
    }
    push(cwd.join(".flint").join("skills"));
    push(config_dir.join("skills"));
    for dir in extra {
        let dir = dir.trim();
        if dir.is_empty() {
            continue;
        }
        let path = PathBuf::from(dir);
        push(if path.is_absolute() { path } else { cwd.join(path) });
    }
    dirs
}

/// Every skill in these directories, first directory first.
///
/// One level deep, exactly: `<dir>/<name>/SKILL.md` and nothing nested below it. A
/// recursive search would find `SKILL.md` files inside a project's fixtures and offer
/// them as procedures, which is worse than finding nothing.
pub fn skills_in(dirs: &[PathBuf]) -> Vec<Skill> {
    let mut out: Vec<Skill> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<Skill> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let file = path.join("SKILL.md");
            if !file.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let (front, body) = split_front_matter(&text);
            let field = |key: &str| {
                front
                    .as_ref()
                    .and_then(|fields| fields.iter().find(|(k, _)| k == key))
                    .map(|(_, v)| v.trim().to_string())
                    .unwrap_or_default()
            };
            let (name, description) = match front {
                Some(_) => {
                    let declared = field("name");
                    let description = {
                        let d = field("description");
                        if d.is_empty() { field("summary") } else { d }
                    };
                    (
                        if declared.is_empty() { name } else { declared },
                        description,
                    )
                }
                None => (name, first_line(body)),
            };
            if name.trim().is_empty() {
                continue;
            }
            found.push(Skill {
                name,
                description,
                path: file,
            });
        }
        // `read_dir` order is arbitrary, and a catalog that reshuffles between requests
        // is one the model cannot learn.
        found.sort_by(|a, b| a.name.cmp(&b.name));
        for skill in found {
            if !out.iter().any(|s| s.name == skill.name) {
                out.push(skill);
            }
        }
    }
    out
}

/// Load one skill's body, by name, from disk.
///
/// The lookup is against the discovered names rather than against a path built from the
/// argument, so there is nothing here for a `../..` to traverse: a name that is not a
/// skill is not a path, it is an error listing the ones that are.
pub fn load_skill(dirs: &[PathBuf], name: &str) -> Result<String> {
    let skills = skills_in(dirs);
    let skill = skills.iter().find(|s| s.name == name).ok_or_else(|| {
        let known: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        anyhow::anyhow!(
            "unknown skill '{}'. Available: {}",
            name,
            if known.is_empty() {
                "(none)".to_string()
            } else {
                known.join(", ")
            }
        )
    })?;
    let text = std::fs::read_to_string(&skill.path)
        .with_context(|| format!("cannot read skill {}", skill.path.display()))?;
    Ok(split_front_matter(&text).1.trim().to_string())
}

/// One prompt template: a saved prompt a person types by name.
///
/// The other half of the skill catalog, and deliberately a different list. A skill is instructions
/// the *model* loads through the `skill` tool; a template is the person's own words, saved, and the
/// model is never told it exists -- nothing about it enters the system prompt or a tool schema, so a
/// directory of long templates costs a run nothing until one is typed.
#[derive(Debug, Clone)]
pub struct Prompt {
    /// The word typed after the slash.
    pub name: String,
    /// One line for `/prompts`: front matter's `description`, or the body's first line.
    pub description: String,
    pub path: PathBuf,
}

/// Which directories are searched for prompt templates, in the order that decides name conflicts.
///
/// The same order and the same reasoning as `skill_dirs_for` -- the project's own prompts win, then
/// the working directory's, then the user's -- with one difference worth stating: the config's extra
/// directories are *not* searched. `skill_dirs` exists because a collection of skills is a thing
/// people already keep in other places; a saved prompt is a sentence about this machine, and a second
/// key saying what `<FLINT_HOME>/prompts/` already says would be one more thing to keep in step.
pub fn prompt_dirs_for(cwd: &Path, config_dir: &Path, root: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    if let Some(root) = root {
        push(root.join(".flint").join("prompts"));
    }
    push(cwd.join(".flint").join("prompts"));
    push(config_dir.join("prompts"));
    dirs
}

/// Every template in these directories, first directory first.
///
/// One level deep, exactly, like the skills and the profiles: `<dir>/<name>.md` and nothing nested,
/// so a draft in a subdirectory is not offered as a command. The file name is the word typed, since
/// that is what a person sees in their own directory listing; `name:` in front matter overrides it
/// for the case where the two should differ, which is the rule the skills already follow.
pub fn prompts_in(dirs: &[PathBuf]) -> Vec<Prompt> {
    let mut out: Vec<Prompt> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<Prompt> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let file_name = path
                .file_stem()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let (front, body) = split_front_matter(&text);
            let field = |key: &str| {
                front
                    .as_ref()
                    .and_then(|fields| fields.iter().find(|(k, _)| k == key))
                    .map(|(_, v)| v.trim().to_string())
                    .unwrap_or_default()
            };
            let name = match front {
                Some(_) => {
                    let declared = field("name");
                    if declared.is_empty() {
                        file_name
                    } else {
                        declared
                    }
                }
                None => file_name,
            };
            let description = {
                let d = field("description");
                if d.is_empty() {
                    first_line(body)
                } else {
                    d
                }
            };
            if name.trim().is_empty() {
                continue;
            }
            found.push(Prompt {
                name,
                description,
                path,
            });
        }
        // `read_dir` order is arbitrary, and a listing that reshuffles between runs is one a person
        // cannot learn -- the same reason the skill catalog sorts.
        found.sort_by(|a, b| a.name.cmp(&b.name));
        for prompt in found {
            if !out.iter().any(|p| p.name == prompt.name) {
                out.push(prompt);
            }
        }
    }
    out
}

/// Load one template's body by name, without its front matter.
///
/// Against the discovered names rather than a path built from the argument, for the same reason as
/// `load_skill`: a name that is not a template is not a path, it is an error listing the ones that
/// are.
pub fn load_prompt(dirs: &[PathBuf], name: &str) -> Result<String> {
    let prompts = prompts_in(dirs);
    let prompt = prompts.iter().find(|p| p.name == name).ok_or_else(|| {
        let known: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
        anyhow::anyhow!(
            "unknown prompt '{}'. Available: {}",
            name,
            if known.is_empty() {
                "(none)".to_string()
            } else {
                known.join(", ")
            }
        )
    })?;
    let text = std::fs::read_to_string(&prompt.path)
        .with_context(|| format!("cannot read prompt {}", prompt.path.display()))?;
    Ok(split_front_matter(&text).1.trim().to_string())
}

/// Put the words typed after a name where the file asked for them.
///
/// `{args}` is the hole, and a file without one gets the words appended as a last paragraph -- which
/// is what "save this prompt and aim it at something else" means when the author did not think about
/// arguments. Both ends are trimmed, and that is not tidiness: a body's own trailing newline would
/// otherwise become a blank line before the appended words, and `{args}` at the end of a sentence
/// must not drag one in. An empty substitution on an empty argument is deliberate, so `/name` with
/// nothing after it sends the file as written rather than dropping the hole and the sentence with it.
pub fn fill_args(body: &str, args: &str) -> String {
    let body = body.trim_end();
    let args = args.trim();
    if body.contains("{args}") {
        body.replace("{args}", args)
    } else if args.is_empty() {
        body.to_string()
    } else {
        format!("{body}\n\n{args}")
    }
}

/// Which directories are searched for agent profiles, in the order that decides name conflicts.
///
/// The same order and the same reasoning as `skill_dirs_for`: the project's own profiles win, then
/// the working directory's, then the user's. A profile is a way to spend money on a model, so a
/// project that ships one means it.
pub fn agent_dirs_for(cwd: &Path, config_dir: &Path, root: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    if let Some(root) = root {
        push(root.join(".flint").join("agents"));
    }
    push(cwd.join(".flint").join("agents"));
    push(config_dir.join("agents"));
    dirs
}

/// Every profile these directories hold, first directory first, then by name.
///
/// One level deep, like the skills: `<dir>/<name>.md` and nothing nested, so a fixture or a draft
/// in a subdirectory is not offered as a way to start a run. A file whose name is `.md` and whose
/// first line is not a fence still counts -- the body is the instructions and the file name is the
/// name -- because a person who writes `explorer.md` with no front matter has written a profile.
pub fn agents_in(dirs: &[PathBuf]) -> Vec<AgentProfile> {
    let mut out: Vec<AgentProfile> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<AgentProfile> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            let stem = path
                .file_stem()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let (front, body) = split_front_matter(&text);
            let mut profile = AgentProfile {
                name: stem,
                description: first_line(body),
                model: None,
                provider: None,
                readonly: false,
                path: path.clone(),
            };
            if let Some(front) = front {
                for (key, value) in front {
                    match key.as_str() {
                        "name" if !value.is_empty() => profile.name = value,
                        "description" | "summary" => profile.description = value,
                        "model" if !value.is_empty() => profile.model = Some(value),
                        "provider" if !value.is_empty() => profile.provider = Some(value),
                        // Only `true` turns it on: an unreadable value must not decide that a child
                        // may write, and the safe reading of "can this write?" is never.
                        "readonly" => profile.readonly = value.eq_ignore_ascii_case("true"),
                        _ => {}
                    }
                }
            }
            if profile.name.trim().is_empty() {
                continue;
            }
            found.push(profile);
        }
        // A catalog that reshuffles between requests is one the model cannot learn, and `read_dir`
        // order is arbitrary.
        found.sort_by(|a, b| a.name.cmp(&b.name));
        for profile in found {
            // First directory wins, so a project's `explorer` is *the* explorer.
            if !out.iter().any(|p| p.name == profile.name) {
                out.push(profile);
            }
        }
    }
    out
}

/// Load one profile's body, by name, from disk.
///
/// Against the discovered names rather than a path built from the argument, for the same reason as
/// `load_skill`: a name that is not a profile is an error listing the ones that are, not a path.
pub fn load_agent_body(dirs: &[PathBuf], name: &str) -> Result<String> {
    let agents = agents_in(dirs);
    let profile = agents
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| unknown_agent_error(name, &agents))?;
    agent_body(profile)
}

/// One profile's body, read from the path discovery already found.
///
/// Separate from the lookup so that a caller holding a profile -- the `task` tool does -- reads the
/// file without searching for it again: a name is resolved against a list once, and a file that
/// appeared between the search and the use cannot turn a name into a path.
pub fn agent_body(profile: &AgentProfile) -> Result<String> {
    let text = std::fs::read_to_string(&profile.path)
        .with_context(|| format!("cannot read agent profile {}", profile.path.display()))?;
    Ok(split_front_matter(&text).1.trim().to_string())
}

/// "there is no such profile" -- with the ones there are, because that is what makes it fixable.
pub fn unknown_agent_error(name: &str, agents: &[AgentProfile]) -> anyhow::Error {
    let known: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    anyhow::anyhow!(
        "unknown agent '{}'. Available: {}. A profile is a file: <project>/.flint/agents/{}.md",
        name,
        if known.is_empty() {
            "(none)".to_string()
        } else {
            known.join(", ")
        },
        name
    )
}

/// Split `---` front matter from the body, as key/value pairs.
///
/// Hand-written rather than pulled from a YAML crate. The format is a handful of known keys, the
/// dependency would exist for ten lines of code, and a parser that cannot be surprised by anchors,
/// tags or multi-document streams is one less thing between a person and their own file.
///
/// Keys are lower-cased and every one is returned, because skills and agent profiles want different
/// ones from the same syntax and a parser per kind of file would be two places to fix. An
/// unrecognised key is ignored by the caller; quoting is trimmed, so `model: "x"` and `model: x`
/// mean the same thing.
fn split_front_matter(text: &str) -> (Option<Vec<(String, String)>>, &str) {
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return (None, text);
    };
    if first.trim() != "---" {
        return (None, text);
    }

    let mut fields: Vec<(String, String)> = Vec::new();
    let mut consumed = first.len() + 1;
    let mut closed = false;
    for line in lines {
        consumed += line.len() + 1;
        if line.trim() == "---" {
            closed = true;
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').to_string();
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        // Last one wins, which is what a person editing a file by hand expects.
        match fields.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => fields.push((key, value)),
        }
    }
    if !closed {
        // An opening fence with no closing one is a document, not front matter.
        return (None, text);
    }
    let body = text.get(consumed..).unwrap_or("");
    (Some(fields), body)
}

/// First non-empty, non-heading line: the fallback description for a skill with no
/// front matter.
fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A temporary tree, removed when the guard drops.
    struct Tree(PathBuf);

    impl Tree {
        fn new(tag: &str) -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "flint-context-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("temp dir");
            Tree(path)
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(&path).expect("temp dir");
            path
        }

        /// Mark a directory as the project root, which is what stops the upward search.
        fn git(&self, rel: &str) -> PathBuf {
            let path = self.dir(rel);
            std::fs::create_dir_all(path.join(".git")).expect("git dir");
            path
        }

        fn file(&self, rel: &str, body: &str) -> PathBuf {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("temp dir");
            }
            std::fs::write(&path, body).expect("write file");
            path
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn workspace(tree: &Tree, cwd: &Path, config_dir: &Path) -> Workspace {
        discover_with(cwd, config_dir, &tree.0.join("home"), &[])
    }

    #[test]
    fn instruction_files_are_collected_from_the_project_root_down_to_the_working_directory() {
        let tree = Tree::new("instructions");
        let project = tree.git("project");
        let nested = tree.dir("project/src/deep");
        tree.file("project/AGENTS.md", "root rules");
        tree.file("project/src/AGENTS.md", "src rules");
        let config_dir = tree.dir("config");

        let found = workspace(&tree, &nested, &config_dir);
        assert_eq!(found.root.as_deref(), Some(project.as_path()));
        let names: Vec<String> = found
            .instruction_files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["AGENTS.md", "AGENTS.md"]);
        // Outermost first: the closest file is read last and therefore wins.
        assert!(found.instruction_files[0].starts_with(&project));
        assert!(found.instruction_files[1].starts_with(project.join("src")));
    }

    /// Above the user's home there is no project, and a stray file there must not steer
    /// every conversation.
    #[test]
    fn the_search_stops_at_the_home_directory() {
        let tree = Tree::new("stop-home");
        let home = tree.dir("home");
        let cwd = tree.dir("home/work");
        tree.file("AGENTS.md", "outside rules");
        tree.file("home/AGENTS.md", "home rules");
        let config_dir = tree.dir("config");

        let found = discover_with(&cwd, &config_dir, &home, &[]);
        assert_eq!(found.instruction_files.len(), 1);
        assert!(found.instruction_files[0].starts_with(&home));
    }

    #[test]
    fn the_user_level_file_is_the_most_general_and_the_project_the_most_specific() {
        let tree = Tree::new("user-file");
        let cwd = tree.dir("work");
        tree.file("work/AGENTS.md", "project rules");
        let config_dir = tree.dir("config");
        tree.file("config/AGENTS.md", "user rules");

        let found = workspace(&tree, &cwd, &config_dir);
        assert_eq!(found.instruction_files.len(), 2);
        assert!(found.instruction_files[0].starts_with(&config_dir));
        assert!(found.instruction_files[1].starts_with(&cwd));
    }

    #[test]
    fn hint_names_the_files_and_paste_includes_them() {
        let tree = Tree::new("modes");
        let cwd = tree.dir("work");
        tree.file("work/AGENTS.md", "Run cargo test before claiming success.\n");
        let config_dir = tree.dir("config");
        let found = workspace(&tree, &cwd, &config_dir);

        let hint = found.prompt_note(Instructions::Hint);
        assert!(hint.contains("AGENTS.md"), "the path is not named: {hint}");
        assert!(
            !hint.contains("Run cargo test"),
            "hint mode pasted the contents: {hint}"
        );

        let paste = found.prompt_note(Instructions::Paste);
        assert!(
            paste.contains("Run cargo test before claiming success."),
            "paste mode did not include the file: {paste}"
        );

        assert!(found.prompt_note(Instructions::Off).is_empty());
    }

    #[test]
    fn a_pasted_file_over_the_budget_is_truncated_on_a_character_boundary() {
        let tree = Tree::new("budget");
        let cwd = tree.dir("work");
        // Multi-byte characters on purpose: cutting mid-character would put a broken
        // sequence in the prompt.
        let body = "é".repeat(PASTE_BUDGET);
        tree.file("work/AGENTS.md", &body);
        let config_dir = tree.dir("config");
        let found = workspace(&tree, &cwd, &config_dir);

        let paste = found.prompt_note(Instructions::Paste);
        assert!(paste.contains("truncated"), "no truncation marker: {paste}");
        assert!(paste.len() < PASTE_BUDGET + 1024);
        assert!(
            std::str::from_utf8(paste.as_bytes()).is_ok(),
            "the pasted prompt is not valid UTF-8"
        );
    }

    #[test]
    fn skills_are_found_one_level_deep_with_their_summaries() {
        let tree = Tree::new("skills");
        let cwd = tree.dir("work");
        let config_dir = tree.dir("config");
        tree.file(
            "work/.flint/skills/tidy-commits/SKILL.md",
            "---\nname: tidy-commits\ndescription: Squash and reword commits.\n---\n\nStep one.\n",
        );
        // A bare markdown file is not a skill, and a nested one is not either.
        tree.file("work/.flint/skills/notes.md", "not a skill");
        tree.file(
            "work/.flint/skills/tidy-commits/nested/SKILL.md",
            "---\nname: nested\n---\n",
        );

        let found = workspace(&tree, &cwd, &config_dir);
        assert_eq!(found.skills.len(), 1, "{:?}", found.skills);
        assert_eq!(found.skills[0].name, "tidy-commits");
        assert_eq!(found.skills[0].description, "Squash and reword commits.");

        let note = found.prompt_note(Instructions::Hint);
        assert!(note.contains("tidy-commits: Squash and reword commits."));
        assert!(
            note.contains("until it has been loaded"),
            "the catalog must say the summaries are not the instructions: {note}"
        );
        assert!(!note.contains("Step one."), "the body leaked into the catalog");
    }

    #[test]
    fn a_skill_without_front_matter_falls_back_to_its_directory_and_first_line() {
        let tree = Tree::new("no-frontmatter");
        let cwd = tree.dir("work");
        let config_dir = tree.dir("config");
        tree.file(
            "work/.flint/skills/release/SKILL.md",
            "# Release\n\nCut the release only from a green main.\n",
        );

        let found = workspace(&tree, &cwd, &config_dir);
        assert_eq!(found.skills.len(), 1);
        assert_eq!(found.skills[0].name, "release");
        assert_eq!(
            found.skills[0].description,
            "Cut the release only from a green main."
        );
    }

    #[test]
    fn the_project_skill_wins_a_name_conflict_and_loading_reads_the_body() {
        let tree = Tree::new("conflict");
        let project = tree.git("project");
        let config_dir = tree.dir("config");
        tree.file(
            "project/.flint/skills/tidy/SKILL.md",
            "---\ndescription: project version\n---\n\nProject body.\n",
        );
        tree.file(
            "config/skills/tidy/SKILL.md",
            "---\ndescription: user version\n---\n\nUser body.\n",
        );

        let found = discover_with(&project, &config_dir, &tree.0.join("home"), &[]);
        assert_eq!(found.skills.len(), 1);
        assert_eq!(found.skills[0].description, "project version");

        let dirs = skill_dirs_for(&project, &config_dir, found.root.as_deref(), &[]);
        let body = load_skill(&dirs, "tidy").expect("load");
        assert_eq!(body, "Project body.");
        let err = format!("{:#}", load_skill(&dirs, "../tidy").unwrap_err());
        assert!(err.contains("unknown skill"), "{err}");
    }

    #[test]
    fn an_extra_skill_directory_from_the_config_is_searched() {
        let tree = Tree::new("extra");
        let cwd = tree.dir("work");
        let config_dir = tree.dir("config");
        tree.file(
            "elsewhere/mine/SKILL.md",
            "---\ndescription: from elsewhere\n---\n",
        );
        let extra = vec![tree.0.join("elsewhere").display().to_string()];

        let found = discover_with(&cwd, &config_dir, &tree.0.join("home"), &extra);
        assert_eq!(found.skills.len(), 1);
        assert_eq!(found.skills[0].description, "from elsewhere");
    }

    #[test]
    fn instruction_mode_names_round_trip_and_reject_nonsense() {
        assert_eq!(Instructions::parse(" hint "), Some(Instructions::Hint));
        assert_eq!(Instructions::parse("PASTE"), Some(Instructions::Paste));
        assert_eq!(Instructions::parse("off"), Some(Instructions::Off));
        assert_eq!(Instructions::parse("sometimes"), None);
        for mode in [Instructions::Off, Instructions::Hint, Instructions::Paste] {
            assert_eq!(Instructions::parse(mode.name()), Some(mode));
        }
    }

    #[test]
    fn an_agent_profile_carries_the_model_readonly_and_provider_it_declares() {
        let tree = Tree::new("agents");
        let cwd = tree.dir("work");
        let config_dir = tree.dir("config");
        tree.file(
            "work/.flint/agents/explorer.md",
            "---\nname: explorer\ndescription: Reads the tree and reports.\nmodel: \"cheap-model\"\n\
             provider: local\nreadonly: true\n---\n\nYou only read. Never write.\n",
        );
        let found = discover_with(&cwd, &config_dir, &tree.0.join("home"), &[]);
        assert_eq!(found.agents.len(), 1, "{:?}", found.agents);
        let profile = &found.agents[0];
        assert_eq!(profile.name, "explorer");
        assert_eq!(profile.description, "Reads the tree and reports.");
        assert_eq!(profile.model.as_deref(), Some("cheap-model"));
        assert_eq!(profile.provider.as_deref(), Some("local"));
        assert!(profile.readonly);
        // The body is read when the profile is used, not when the catalog is built: a directory of
        // long profiles must not put all of their text in every request.
        let (_, body) = found.load_agent("explorer").expect("load");
        assert_eq!(body, "You only read. Never write.");
    }

    #[test]
    fn a_profile_with_no_front_matter_is_named_after_its_file_and_summarised_by_its_first_line() {
        let tree = Tree::new("agents-bare");
        let cwd = tree.dir("work");
        let config_dir = tree.dir("config");
        tree.file(
            "work/.flint/agents/sweeper.md",
            "# Sweeper\n\nTidy the tree and say what moved.\n",
        );
        let found = discover_with(&cwd, &config_dir, &tree.0.join("home"), &[]);
        assert_eq!(found.agents.len(), 1);
        assert_eq!(found.agents[0].name, "sweeper");
        assert_eq!(
            found.agents[0].description,
            "Tidy the tree and say what moved."
        );
        assert!(!found.agents[0].readonly);
        assert!(found.agents[0].model.is_none());
    }

    #[test]
    fn readonly_is_turned_on_only_by_saying_true() {
        // The safe reading of "may this child write?" is never, so an unreadable value cannot
        // decide it. `yes` is a reasonable thing to write and is not a word this parser knows.
        let tree = Tree::new("agents-readonly");
        let cwd = tree.dir("work");
        let config_dir = tree.dir("config");
        tree.file("work/.flint/agents/yes.md", "---\nreadonly: yes\n---\nbody\n");
        tree.file("work/.flint/agents/on.md", "---\nreadonly: TRUE\n---\nbody\n");
        let found = discover_with(&cwd, &config_dir, &tree.0.join("home"), &[]);
        let by_name = |name: &str| {
            found
                .agents
                .iter()
                .find(|a| a.name == name)
                .expect("profile")
                .readonly
        };
        assert!(!by_name("yes"));
        assert!(by_name("on"));
    }

    #[test]
    fn the_projects_profile_wins_over_the_users_and_a_name_is_never_a_path() {
        let tree = Tree::new("agents-order");
        let project = tree.git("project");
        let config_dir = tree.dir("config");
        tree.file(
            "config/agents/explorer.md",
            "---\ndescription: user version\n---\nUser body.\n",
        );
        tree.file(
            "project/.flint/agents/explorer.md",
            "---\ndescription: project version\n---\nProject body.\n",
        );
        let found = discover_with(&project, &config_dir, &tree.0.join("home"), &[]);
        assert_eq!(found.agents.len(), 1, "{:?}", found.agents);
        assert_eq!(found.agents[0].description, "project version");

        let dirs = agent_dirs_for(&project, &config_dir, found.root.as_deref());
        let (profile, body) = found.load_agent("explorer").expect("load");
        assert_eq!(profile.description, "project version");
        assert_eq!(body, "Project body.");
        assert!(load_agent_body(&dirs, "explorer").is_ok());

        let err = format!("{:#}", load_agent_body(&dirs, "../../etc/passwd").unwrap_err());
        assert!(err.contains("unknown agent"), "{err}");
        assert!(err.contains("explorer"), "the error does not say what exists: {err}");
    }

    #[test]
    fn a_profile_summary_says_what_changes_behaviour_and_nothing_else() {
        let plain = AgentProfile {
            name: "reader".to_string(),
            description: String::new(),
            model: None,
            provider: None,
            readonly: false,
            path: PathBuf::from("x.md"),
        };
        assert_eq!(plain.summary(), "reader");
        let loud = AgentProfile {
            description: "Looks, does not touch.".to_string(),
            model: Some("cheap".to_string()),
            provider: Some("local".to_string()),
            readonly: true,
            ..plain
        };
        assert_eq!(
            loud.summary(),
            "reader — Looks, does not touch. [readonly, model cheap, provider local]"
        );
    }

    #[test]
    fn a_prompt_is_a_markdown_file_one_level_deep_named_by_its_file() {
        let tree = Tree::new("prompts");
        let cwd = tree.dir("work");
        let config_dir = tree.dir("config");
        tree.file(
            "config/prompts/tidy-commits.md",
            "---\ndescription: Squash and reword the commits.\n---\n\nTidy {args}.\n",
        );
        // The file name is the word typed, and front matter may override it; a `.txt`, a nested file
        // and a directory are not prompts, for the same reason a nested `SKILL.md` is not a skill.
        tree.file("config/prompts/release.md", "---\nname: cut-release\n---\nCut it.\n");
        tree.file("config/prompts/notes.txt", "not a prompt");
        tree.file("config/prompts/deep/draft.md", "not a prompt either");

        let found = discover_with(&cwd, &config_dir, &tree.0.join("home"), &[]);
        let names: Vec<&str> = found.prompts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["cut-release", "tidy-commits"], "{:?}", found.prompts);
        assert_eq!(found.prompts[1].description, "Squash and reword the commits.");
        // No front matter at all: a person who writes `foo.md` and nothing else has written a prompt,
        // the same reading the profiles and the skills take.
        assert_eq!(found.prompts[0].description, "Cut it.");
        assert_eq!(found.prompts[0].path, config_dir.join("prompts/release.md"));
    }

    #[test]
    fn the_projects_prompt_wins_over_the_users_and_a_name_is_never_a_path() {
        let tree = Tree::new("prompts-order");
        let project = tree.git("project");
        let config_dir = tree.dir("config");
        tree.file("config/prompts/tidy-commits.md", "---\n---\nUser words.\n");
        tree.file(
            "project/.flint/prompts/tidy-commits.md",
            "---\n---\nProject words.\n",
        );
        let found = discover_with(&project, &config_dir, &tree.0.join("home"), &[]);
        assert_eq!(found.prompts.len(), 1, "{:?}", found.prompts);
        assert_eq!(
            found.load_prompt("tidy-commits").expect("load"),
            "Project words."
        );
        assert!(found.prompt("tidy-commits").is_some());
        assert!(found.prompt("nothing-by-that-name").is_none());

        let err = format!("{:#}", found.load_prompt("../../etc/passwd").unwrap_err());
        assert!(err.contains("unknown prompt"), "{err}");
        assert!(err.contains("tidy-commits"), "the error does not say what exists: {err}");
    }

    #[test]
    fn arguments_fill_the_hole_and_are_appended_when_there_is_none() {
        // The hole, filled and trimmed: what the person typed is read as one argument however much
        // whitespace they put around it, and `{args}` in the middle of a sentence does not drag a
        // newline into it.
        assert_eq!(
            fill_args("Tidy {args}, then stop.\n", "  src/parser.rs \n"),
            "Tidy src/parser.rs, then stop."
        );
        // No hole: the words become a last paragraph, and the body's own trailing newline is not
        // allowed to become a blank line first.
        assert_eq!(
            fill_args("Summarise the README.\n", "in five lines"),
            "Summarise the README.\n\nin five lines"
        );
        // Nothing typed: the file is sent as written, with no empty paragraph and no `{args}` left in
        // a sentence that asked for one.
        assert_eq!(fill_args("Summarise the README.\n", ""), "Summarise the README.");
        assert_eq!(fill_args("Tidy {args}.", ""), "Tidy .");
    }
}
