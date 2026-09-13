//! The patch format, and applying it to file contents.
//!
//! `*** Begin Patch` / `*** Add File:` / `*** Update File:` / `*** Delete File:` /
//! `*** End Patch`, the format Codex uses. Three properties are worth more than the
//! syntax itself, and each of them is a decision:
//!
//! * **A hunk is located by its own lines.** There is no line-number arithmetic to get
//!   wrong, and no way to apply a change to the wrong place because a file grew by three
//!   lines since it was read. If the context and the removed lines are not in the file
//!   exactly once, the patch does not apply, and says which hunk and which file.
//! * **Nothing is written until everything matches.** Several files in one patch are one
//!   edit: a bad hunk in the third file leaves the first two untouched, rather than
//!   leaving the tree half-changed and the model to work out which half.
//! * **The parsing is separate from the writing.** [`parse`] is a pure function over
//!   text, and [`apply_to`] is a pure function over one file's contents, so the format is
//!   testable without a filesystem and the tool that does the IO has nothing to decide.
//!
//! Deliberately not supported: `*** Move to:` (a rename, which is a delete and an add and
//! a chance to lose a file), and fuzzy matching of any kind. A patch that does not fit is
//! a patch that needs to be rewritten against the file as it is.

use anyhow::{anyhow, bail, Result};
use std::path::Path;

/// One file's worth of the patch.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Add { path: String, content: String },
    Update { path: String, hunks: Vec<Hunk> },
    Delete { path: String },
}

impl Change {
    pub fn path(&self) -> &str {
        match self {
            Change::Add { path, .. } | Change::Update { path, .. } | Change::Delete { path } => path,
        }
    }

    /// What to call this in a summary line: `add`, `update`, `delete`.
    pub fn verb(&self) -> &'static str {
        match self {
            Change::Add { .. } => "add",
            Change::Update { .. } => "update",
            Change::Delete { .. } => "delete",
        }
    }
}

/// A contiguous replacement: lines that must be there, and lines that replace them.
#[derive(Debug, Clone, PartialEq)]
pub struct Hunk {
    /// Lines expected in the file: context lines and removed lines, in order.
    pub old: Vec<String>,
    /// Lines that take their place: context lines and added lines, in order.
    pub new: Vec<String>,
}

pub fn parse(text: &str) -> Result<Vec<Change>> {
    let mut lines = text.lines().enumerate().peekable();

    // Anything before `*** Begin Patch` is not part of the patch -- a model that explains
    // itself first is common, and refusing the whole call over it helps nobody.
    let mut began = false;
    for (_, line) in lines.by_ref() {
        if line.trim() == "*** Begin Patch" {
            began = true;
            break;
        }
    }
    if !began {
        bail!("patch must start with a line reading `*** Begin Patch`");
    }

    let mut changes: Vec<Change> = Vec::new();
    let mut ended = false;

    while let Some((number, line)) = lines.next() {
        let line = line.trim_end();
        if line.trim() == "*** End Patch" {
            ended = true;
            break;
        }
        let Some((kind, path)) = split_directive(line) else {
            bail!(
                "line {}: expected `*** Add File:`, `*** Update File:`, `*** Delete File:` or \
                 `*** End Patch`, found: {line}",
                number + 1
            );
        };
        if path.is_empty() {
            bail!("line {}: `{kind}` needs a file path", number + 1);
        }

        match kind.as_str() {
            "Add File" => {
                let mut body = Vec::new();
                while let Some((_, next)) = lines.peek() {
                    if is_directive(next) {
                        break;
                    }
                    let (_, next) = lines.next().expect("peeked");
                    let Some(added) = next.strip_prefix('+') else {
                        bail!(
                            "line {}: every line of an added file must start with `+`",
                            number + 1
                        );
                    };
                    body.push(added.to_string());
                }
                changes.push(Change::Add {
                    path: path.to_string(),
                    content: join_lines(&body),
                });
            }
            "Delete File" => {
                changes.push(Change::Delete {
                    path: path.to_string(),
                });
            }
            "Update File" => {
                let hunks = parse_hunks(&mut lines)?;
                if hunks.is_empty() {
                    bail!("`Update File: {path}` has no changes");
                }
                changes.push(Change::Update {
                    path: path.to_string(),
                    hunks,
                });
            }
            // `split_directive` only ever returns the three kinds above, so this arm is
            // unreachable; it is here because matching on a string has to be exhaustive.
            other => bail!("unsupported patch directive `*** {other}:`"),
        }
    }

    if !ended {
        bail!("patch must end with a line reading `*** End Patch`");
    }
    if changes.is_empty() {
        bail!("the patch is empty: it has a start and an end but no files in between");
    }
    Ok(changes)
}

/// `*** Update File:` hunks, up to the next directive.
fn parse_hunks<'a, I>(lines: &mut std::iter::Peekable<I>) -> Result<Vec<Hunk>>
where
    I: Iterator<Item = (usize, &'a str)>,
{
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut old: Vec<String> = Vec::new();
    let mut new: Vec<String> = Vec::new();
    let mut in_hunk = false;

    // Each `@@` line starts a hunk. The text after it is a hint for a human reading the
    // patch, and is not used to find anything: the lines themselves are the location.
    let flush = |old: &mut Vec<String>,
                     new: &mut Vec<String>,
                     in_hunk: &mut bool,
                     hunks: &mut Vec<Hunk>|
     -> Result<()> {
        if !*in_hunk {
            return Ok(());
        }
        if old.is_empty() {
            bail!("a hunk has no context and nothing to remove, so it has no location");
        }
        hunks.push(Hunk {
            old: std::mem::take(old),
            new: std::mem::take(new),
        });
        *in_hunk = false;
        Ok(())
    };

    while let Some((number, line)) = lines.peek().copied() {
        let trimmed = line.trim_end();
        if is_directive(trimmed) {
            break;
        }
        lines.next();
        if let Some(rest) = trimmed.strip_prefix("@@") {
            let _ = rest;
            flush(&mut old, &mut new, &mut in_hunk, &mut hunks)?;
            in_hunk = true;
            continue;
        }
        // A patch with no `@@` at all is a single hunk, which is how a small edit to one
        // file is usually written.
        in_hunk = true;
        if let Some(rest) = trimmed.strip_prefix('+') {
            new.push(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix('-') {
            old.push(rest.to_string());
        } else if let Some(rest) = trimmed.strip_prefix(' ') {
            old.push(rest.to_string());
            new.push(rest.to_string());
        } else if trimmed.is_empty() {
            // A blank line in a patch is a blank context line: it is far more common than
            // sending a line consisting of a single space, and treating it as a format
            // error would reject patches that are perfectly clear.
            old.push(String::new());
            new.push(String::new());
        } else {
            bail!(
                "line {}: every line inside a hunk must start with `+`, `-` or a space, \
                 found: {trimmed}",
                number + 1
            );
        }
    }
    flush(&mut old, &mut new, &mut in_hunk, &mut hunks)?;
    Ok(hunks)
}

fn is_directive(line: &str) -> bool {
    let line = line.trim_end();
    line.trim() == "*** End Patch" || split_directive(line).is_some()
}

/// `*** Update File: src/main.rs` into `("Update File", "src/main.rs")`.
fn split_directive(line: &str) -> Option<(String, &str)> {
    let rest = line.strip_prefix("*** ")?;
    let (kind, path) = match rest.split_once(':') {
        Some((kind, path)) => (kind.trim(), path.trim()),
        None => (rest.trim(), ""),
    };
    match kind {
        "Add File" | "Update File" | "Delete File" => Some((kind.to_string(), path)),
        _ => None,
    }
}

fn join_lines(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// Apply one file's change to what the file holds now (`None` when it is not there).
///
/// Returns the contents the file should have, or `None` for a deletion.
pub fn apply_to(change: &Change, path: &Path, current: Option<&str>) -> Result<Option<String>> {
    match change {
        Change::Add { content, .. } => {
            if current.is_some() {
                bail!(
                    "cannot add {}: the file already exists. Use `*** Update File:` to change it",
                    path.display()
                );
            }
            Ok(Some(content.clone()))
        }
        Change::Delete { .. } => {
            if current.is_none() {
                bail!("cannot delete {}: the file does not exist", path.display());
            }
            Ok(None)
        }
        Change::Update { hunks, .. } => {
            let Some(current) = current else {
                bail!(
                    "cannot update {}: the file does not exist",
                    path.display()
                );
            };
            let mut text = current.to_string();
            for (index, hunk) in hunks.iter().enumerate() {
                text = apply_hunk(&text, hunk).map_err(|e| {
                    anyhow!("{} (hunk {} of {})", e, index + 1, path.display())
                })?;
            }
            Ok(Some(text))
        }
    }
}

/// Replace one hunk's old lines with its new lines, refusing to guess.
fn apply_hunk(text: &str, hunk: &Hunk) -> Result<String> {
    let lines: Vec<&str> = split_keeping_nothing(text);
    let whole: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let old = &hunk.old;

    let mut found: Vec<usize> = Vec::new();
    if old.len() <= whole.len() {
        for start in 0..=(whole.len() - old.len()) {
            if whole[start..start + old.len()] == old[..] {
                found.push(start);
            }
        }
    }

    match found.len() {
        0 => bail!(
            "the text to replace is not in the file. Read the file and write the patch \
             against what is actually there"
        ),
        1 => {}
        n => bail!(
            "the text to replace appears {n} times; include more surrounding lines to say \
             which one"
        ),
    }

    let start = found[0];
    let mut out: Vec<String> = whole[..start].to_vec();
    out.extend(hunk.new.iter().cloned());
    out.extend(whole[start + old.len()..].iter().cloned());
    Ok(join_lines(&out))
}

/// Split on `\n`, dropping the terminator, and keeping the fact that the file ended with
/// one out of the comparison: a missing final newline is not a change to any line.
fn split_keeping_nothing(text: &str) -> Vec<&str> {
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("file.txt")
    }

    #[test]
    fn an_added_file_carries_its_lines() {
        let changes = parse(
            "*** Begin Patch\n\
             *** Add File: hello.txt\n\
             +one\n\
             +two\n\
             *** End Patch\n",
        )
        .expect("parse");
        assert_eq!(
            changes,
            vec![Change::Add {
                path: "hello.txt".to_string(),
                content: "one\ntwo\n".to_string(),
            }]
        );
        assert_eq!(
            apply_to(&changes[0], &path(), None).unwrap(),
            Some("one\ntwo\n".to_string())
        );
    }

    /// A model that introduces the patch in prose, or wraps it in a code fence it forgot
    /// to close, is still sending a patch. Refusing the call over the preamble would cost
    /// a turn and teach it only to be luckier next time.
    #[test]
    fn text_before_the_patch_is_ignored() {
        let changes = parse(
            "Here is the patch you asked for:\n\
             *** Begin Patch\n\
             *** Delete File: gone.txt\n\
             *** End Patch\n",
        )
        .expect("parse");
        assert_eq!(
            changes,
            vec![Change::Delete {
                path: "gone.txt".to_string()
            }]
        );
    }

    #[test]
    fn an_update_replaces_the_lines_it_names() {
        let changes = parse(
            "*** Begin Patch\n\
             *** Update File: file.txt\n\
             @@ fn main() @@\n\
             \x20let a = 1;\n\
             -let b = 2;\n\
             +let b = 3;\n\
             \x20let c = 4;\n\
             *** End Patch\n",
        )
        .expect("parse");
        assert_eq!(changes.len(), 1);
        let current = "let a = 1;\nlet b = 2;\nlet c = 4;\n";
        let out = apply_to(&changes[0], &path(), Some(current))
            .expect("apply")
            .unwrap();
        assert_eq!(out, "let a = 1;\nlet b = 3;\nlet c = 4;\n");
    }

    /// Context is what locates a hunk, so the same old lines in two places are an error
    /// rather than a coin toss: the patch has to say which one it means.
    #[test]
    fn an_ambiguous_hunk_is_refused() {
        let changes = parse(
            "*** Begin Patch\n\
             *** Update File: file.txt\n\
             -same\n\
             +different\n\
             *** End Patch\n",
        )
        .expect("parse");
        let current = "same\nsame\n";
        let err = format!("{:#}", apply_to(&changes[0], &path(), Some(current)).unwrap_err());
        assert!(err.contains("appears 2 times"), "{err}");
    }

    #[test]
    fn a_hunk_that_does_not_match_says_so_and_names_the_hunk() {
        let changes = parse(
            "*** Begin Patch\n\
             *** Update File: file.txt\n\
             -not in the file\n\
             +whatever\n\
             *** End Patch\n",
        )
        .expect("parse");
        let err = format!(
            "{:#}",
            apply_to(&changes[0], &path(), Some("something else\n")).unwrap_err()
        );
        assert!(err.contains("not in the file"), "{err}");
        assert!(err.contains("hunk 1 of file.txt"), "{err}");
    }

    /// Two hunks in one file, applied in order and independently located.
    #[test]
    fn two_hunks_in_one_file_both_apply() {
        let changes = parse(
            "*** Begin Patch\n\
             *** Update File: file.txt\n\
             @@ first @@\n\
             -one\n\
             +ONE\n\
             @@ second @@\n\
             -three\n\
             +THREE\n\
             *** End Patch\n",
        )
        .expect("parse");
        let out = apply_to(&changes[0], &path(), Some("one\ntwo\nthree\n"))
            .expect("apply")
            .unwrap();
        assert_eq!(out, "ONE\ntwo\nTHREE\n");
    }

    /// A blank line inside a hunk is a blank line in the file, and it is context: it has
    /// to be there for the hunk to match, and it stays. Requiring a literal `+` before it
    /// would reject the shape most real patches have.
    #[test]
    fn a_blank_line_is_context() {
        let changes = parse(
            "*** Begin Patch\n\
             *** Update File: file.txt\n\
             -a\n\
             \n\
             -b\n\
             +c\n\
             *** End Patch\n",
        )
        .expect("parse");
        // The blank line is in both halves, so it remains; `a` and `b` are what change.
        let out = apply_to(&changes[0], &path(), Some("a\n\nb\n"))
            .expect("apply")
            .unwrap();
        assert_eq!(out, "\nc\n");

        // And it is required: without it the hunk is not the text it says it is.
        assert!(
            apply_to(&changes[0], &path(), Some("a\nb\n")).is_err(),
            "a hunk matched a file whose blank context line was missing"
        );
    }

    #[test]
    fn more_than_one_file_is_one_patch() {
        let changes = parse(
            "*** Begin Patch\n\
             *** Add File: new.txt\n\
             +hello\n\
             *** Update File: old.txt\n\
             -before\n\
             +after\n\
             *** Delete File: dead.txt\n\
             *** End Patch\n",
        )
        .expect("parse");
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].verb(), "add");
        assert_eq!(changes[1].verb(), "update");
        assert_eq!(changes[2].verb(), "delete");
    }

    #[test]
    fn adding_a_file_that_exists_is_refused() {
        let changes = parse(
            "*** Begin Patch\n\
             *** Add File: file.txt\n\
             +one\n\
             *** End Patch\n",
        )
        .expect("parse");
        let err = format!(
            "{:#}",
            apply_to(&changes[0], &path(), Some("already here\n")).unwrap_err()
        );
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn malformed_patches_are_refused_with_the_line_number() {
        for (label, text, expected) in [
            ("no start", "*** End Patch\n", "must start with"),
            ("no end", "*** Begin Patch\n*** Add File: x\n+1\n", "must end with"),
            ("empty", "*** Begin Patch\n*** End Patch\n", "the patch is empty"),
            (
                "unknown directive",
                "*** Begin Patch\n*** Frobnify File: x\n*** End Patch\n",
                "expected `*** Add File:`",
            ),
            (
                "add line without plus",
                "*** Begin Patch\n*** Add File: x\nplain\n*** End Patch\n",
                "must start with `+`",
            ),
            (
                "update with nothing",
                "*** Begin Patch\n*** Update File: x\n*** End Patch\n",
                "has no changes",
            ),
            (
                "hunk line without a marker",
                "*** Begin Patch\n*** Update File: x\nplain\n*** End Patch\n",
                "must start with `+`, `-` or a space",
            ),
            (
                "directive without a path",
                "*** Begin Patch\n*** Add File:\n+x\n*** End Patch\n",
                "needs a file path",
            ),
        ] {
            let err = format!("{:#}", parse(text).unwrap_err());
            assert!(
                err.contains(expected),
                "{label}: expected {expected:?}, got {err:?}"
            );
        }
    }
}
