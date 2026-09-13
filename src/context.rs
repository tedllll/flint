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
    Workspace {
        root,
        instruction_files,
        skills: skills_in(&dirs),
        skill_dirs: dirs,
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
            let (name, description) = match front {
                Some((n, d)) => (
                    if n.trim().is_empty() { name } else { n.trim().to_string() },
                    d.trim().to_string(),
                ),
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

/// Split `---` front matter from the body.
///
/// Hand-written rather than pulled from a YAML crate. The format is two known keys, the
/// dependency would exist for ten lines of code, and a parser that cannot be surprised
/// by anchors, tags or multi-document streams is one less thing between a person and
/// their own file. An unrecognised key is ignored; a missing one falls back to the
/// directory name and the first line of the body.
fn split_front_matter(text: &str) -> (Option<(String, String)>, &str) {
    let mut lines = text.lines();
    let Some(first) = lines.next() else {
        return (None, text);
    };
    if first.trim() != "---" {
        return (None, text);
    }

    let mut name = String::new();
    let mut description = String::new();
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
        match key.trim().to_ascii_lowercase().as_str() {
            "name" => name = value,
            "description" | "summary" => description = value,
            _ => {}
        }
    }
    if !closed {
        // An opening fence with no closing one is a document, not front matter.
        return (None, text);
    }
    let body = text.get(consumed..).unwrap_or("");
    (Some((name, description)), body)
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
}
