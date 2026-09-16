//! `@path` in a prompt: the file's contents, inlined before the request.
//!
//! This is the answer to a measured wall rather than a convenience. On Windows a command line is
//! capped at about 32k characters, and the cap is enforced by `CreateProcess`: a 33k prompt fails
//! with `FileNotFoundError [WinError 206]` *in the caller's own `subprocess` call*, before flint is
//! even started. So a document, a rule table or a diff cannot be handed to flint as an argument at
//! all -- and being told the model should "read the file" is not the same thing: a path is a
//! request the model may decline, `read` returns 2000 lines by default, and a long tool result is
//! spilled to a file past `max_tool_output`. Content that has to be *seen* has to be in the prompt.
//!
//! The rule is deliberately small, because a prompt is prose and `@` is an ordinary character in it:
//!
//! - A name is a candidate when it is `@` followed by a word, or by a quoted name that may contain
//!   spaces. An `@` *inside* a word is not a candidate, so `someone@example.com` is an address and
//!   not a file called `example.com`.
//! - A candidate is replaced only if it names a file that can be read as text. Everything else is
//!   left exactly as typed: `@bob` in a sentence is a sentence, and a mistyped path is a mistyped
//!   path rather than a run that refuses to start over a character in prose. What *was* inlined is
//!   on the stream as `turn.started`'s `attachments`, so a caller can check rather than hope.
//! - A name that is not a file is tried once more without trailing punctuation, so `@a.txt.` at the
//!   end of a sentence works and the full stop stays a full stop -- but only when what is left of
//!   the name really is a file, so a file actually called `a.txt.` still wins.
//!
//! Expansion happens for a one-shot prompt (`-p`) and not for a line typed into the REPL. The
//! caller's problem is that an argument cannot carry a document; a person at a terminal can see the
//! file, and a conversation that silently grew by a megabyte is a surprise in the one place where
//! nobody asked for one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// How much may be inlined into one prompt, in bytes, across all the files in it.
///
/// Not a ration, and not a claim about what a model can read: 256 KB of text is roughly 65k tokens,
/// which is already past what most endpoints accept in a single request. The number exists because
/// both of the alternatives are worse. Sending it and letting the endpoint refuse tells the caller
/// after the run, with a provider's error instead of flint's; and an endpoint that silently
/// truncates turns an oversized document into a *wrong answer that looks complete* -- the exact
/// failure this feature is for. Refusing names the file and happens before anything is sent.
pub const MAX_INLINED_BYTES: usize = 256 * 1024;

/// Punctuation that a sentence may leave attached to a name.
const TRAILING: &[char] = &[
    '.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\'', '`', '*',
];

/// One file inlined into a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// The name as the caller wrote it, `@` and quotes included: what a message about it should say.
    pub token: String,
    /// Where the contents were read from, resolved against the working directory.
    pub path: PathBuf,
    pub bytes: usize,
    pub lines: usize,
}

impl Attachment {
    /// How this attachment reads in a person's transcript.
    pub fn describe(&self) -> String {
        format!(
            "{} ({} bytes, {} line{})",
            self.token,
            self.bytes,
            self.lines,
            if self.lines == 1 { "" } else { "s" }
        )
    }
}

/// A prompt, before and after expansion.
#[derive(Debug, Clone)]
pub struct Prompt {
    /// What was typed. What the stream and the transcript show, because the turn is the caller's
    /// words -- a frame carrying a whole attached document would be the stream paying twice for it.
    /// The session file records the expanded form, which is what the model was actually given.
    pub typed: String,
    /// What the model is given: the typed words with each named file's contents in place of its name.
    pub sent: String,
    pub attachments: Vec<Attachment>,
}

/// A name that may be a file, as found in the prompt.
struct Candidate {
    /// Byte offset of the `@`.
    start: usize,
    /// Byte offset just past the name (and past its quotes, when it was quoted).
    end: usize,
    /// The name to resolve, without the `@` or the quotes.
    name: String,
    /// Whether it was written as a quoted name, which is a promise about where the name ends.
    quoted: bool,
}

/// Replace every `@file` in `typed` with that file's contents.
pub fn expand(typed: &str, cwd: &Path) -> Result<Prompt> {
    let mut sent = String::with_capacity(typed.len());
    let mut attachments: Vec<Attachment> = Vec::new();
    let mut total = 0usize;
    let mut copied = 0usize;

    for candidate in candidates(typed) {
        // The name as written, for anything a person or a caller will read. The *replaced* range is
        // recomputed below when trailing punctuation turned out to be prose rather than a name.
        let token = typed[candidate.start..candidate.end].to_string();
        let mut name = candidate.name.clone();
        // A name is looked up without trailing punctuation first, and only then as written. The
        // ordering matters on Windows, where the filesystem API strips a trailing dot or space: there
        // `@a.txt.` "works" while producing a tag naming `a.txt.`, which resolves nowhere else. Asking
        // the trimmed question first makes one prompt mean one file on every platform, and what is
        // left over is prose again. A name that really does end in a dot (possible on Unix) still
        // wins, because the trimmed form is then not a file at all.
        let mut path = resolve(cwd, &name);
        if !candidate.quoted {
            let trimmed = name.trim_end_matches(TRAILING);
            if trimmed.len() != name.len() {
                let shorter = resolve(cwd, trimmed);
                if shorter.is_file() {
                    path = shorter;
                    name = trimmed.to_string();
                }
            }
        }
        if !path.is_file() {
            continue;
        }

        let bytes = std::fs::read(&path)
            .with_context(|| format!("{token} could not be read"))?;
        total += bytes.len();
        if total > MAX_INLINED_BYTES {
            return Err(anyhow::anyhow!(
                "{token} would put more than {} bytes of files into one prompt (the limit is {} bytes \
                 and {} {} already attached). A model's context is the real limit here, and an \
                 endpoint that truncates silently would turn this into a wrong answer instead of an \
                 error: attach less, or let the model read it -- `read` takes an offset and a limit",
                total,
                MAX_INLINED_BYTES,
                attachments.len(),
                if attachments.len() == 1 { "file is" } else { "files are" },
            ));
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                return Err(anyhow::anyhow!(
                    "{token} is not text (it is not valid UTF-8), so it cannot go into a prompt. \
                     A picture or a binary is not something to inline -- describe it, or have the \
                     model work with it through a tool"
                ))
            }
        };
        // A byte-order mark is the encoding's business, not the content's: a model shown `\u{feff}`
        // at the top of a document spends its first tokens on an invisible character.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        let lines = text.lines().count();
        // Where the replacement ends: a quoted name carries its quotes with it, and an unquoted one
        // ends where the (possibly trimmed) name ends, so punctuation left behind stays in the prose.
        let end = if candidate.quoted {
            candidate.end
        } else {
            candidate.start + 1 + name.len()
        };

        sent.push_str(&typed[copied..candidate.start]);
        sent.push_str(&format!(
            "<file path=\"{name}\" lines=\"{lines}\">\n{text}{}</file>",
            if text.ends_with('\n') { "" } else { "\n" }
        ));
        copied = end;
        attachments.push(Attachment {
            token,
            path,
            bytes: text.len(),
            lines,
        });
    }

    sent.push_str(&typed[copied..]);
    Ok(Prompt {
        typed: typed.to_string(),
        sent,
        attachments,
    })
}

/// The name as a path: a name that is already absolute is taken as written, everything else is
/// relative to the working directory -- which is where the model's own tools resolve it.
fn resolve(cwd: &Path, name: &str) -> PathBuf {
    let path = Path::new(name);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

/// Every `@name` in the text, in the order it appears.
fn candidates(text: &str) -> Vec<Candidate> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut found = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i].1 != '@' || (i > 0 && is_word(chars[i - 1].1)) {
            i += 1;
            continue;
        }
        let start = chars[i].0;
        let open = i + 1;
        let Some(&(_, first)) = chars.get(open) else {
            break;
        };
        if first == '"' || first == '\'' {
            let mut close = None;
            let mut j = open + 1;
            while j < chars.len() {
                if chars[j].1 == '\n' {
                    break;
                }
                if chars[j].1 == first {
                    close = Some(j);
                    break;
                }
                j += 1;
            }
            // An unterminated quote is prose, not a name that swallowed the rest of the line.
            let Some(close) = close else {
                i = open;
                continue;
            };
            let name: String = chars[open + 1..close].iter().map(|&(_, c)| c).collect();
            if !name.trim().is_empty() {
                let end = chars[close].0 + first.len_utf8();
                found.push(Candidate {
                    start,
                    end,
                    name,
                    quoted: true,
                });
            }
            i = close + 1;
            continue;
        }
        let mut j = open;
        while j < chars.len() && !chars[j].1.is_whitespace() {
            j += 1;
        }
        let name: String = chars[open..j].iter().map(|&(_, c)| c).collect();
        if !name.is_empty() {
            let end = if j < chars.len() { chars[j].0 } else { text.len() };
            found.push(Candidate {
                start,
                end,
                name,
                quoted: false,
            });
        }
        i = j;
    }
    found
}

/// Whether a character can be part of a word, for the rule that an `@` in the middle of one is not
/// the start of a name.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("flint-attach-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&path).expect("scratch directory");
        path
    }

    fn write(cwd: &Path, name: &str, body: &str) -> PathBuf {
        let path = cwd.join(name);
        std::fs::write(&path, body).expect("the fixture file");
        path
    }

    #[test]
    fn the_file_replaces_its_name_and_the_prose_survives() {
        let cwd = dir("replace");
        write(&cwd, "a.txt", "first\nsecond\n");

        let prompt = expand("compare @a.txt with what I said", &cwd).expect("no refusal");

        assert_eq!(prompt.typed, "compare @a.txt with what I said");
        assert_eq!(
            prompt.sent,
            "compare <file path=\"a.txt\" lines=\"2\">\nfirst\nsecond\n</file> with what I said"
        );
        assert_eq!(prompt.attachments.len(), 1);
        assert_eq!(prompt.attachments[0].token, "@a.txt");
        assert_eq!(prompt.attachments[0].lines, 2);
        assert_eq!(prompt.attachments[0].bytes, "first\nsecond\n".len());
        assert!(prompt.attachments[0].path.is_file());
    }

    #[test]
    fn a_file_without_a_final_newline_still_ends_its_block_on_its_own_line() {
        let cwd = dir("nonewline");
        write(&cwd, "a.txt", "one line, no newline");

        let prompt = expand("@a.txt", &cwd).expect("no refusal");

        assert_eq!(
            prompt.sent,
            "<file path=\"a.txt\" lines=\"1\">\none line, no newline\n</file>"
        );
    }

    #[test]
    fn an_at_that_names_nothing_is_left_exactly_as_typed() {
        let cwd = dir("prose");
        // Nothing here is a file: an address, a handle, a decorator, and a name nobody wrote.
        let typed = "ask @bob, mail someone@example.com about @decorator and @missing.txt";
        let prompt = expand(typed, &cwd).expect("no refusal");

        assert_eq!(prompt.sent, typed, "prose was rewritten");
        assert!(prompt.attachments.is_empty());
    }

    #[test]
    fn trailing_punctuation_is_not_part_of_the_name() {
        let cwd = dir("punctuation");
        write(&cwd, "a.txt", "body");

        let prompt = expand("see @a.txt.", &cwd).expect("no refusal");

        assert_eq!(
            prompt.sent,
            "see <file path=\"a.txt\" lines=\"1\">\nbody\n</file>.",
            "the full stop belongs to the sentence"
        );
    }

    #[test]
    fn a_name_may_be_quoted_when_it_has_spaces_in_it() {
        let cwd = dir("quoted");
        write(&cwd, "two words.txt", "spaced");

        let prompt = expand("read @\"two words.txt\" now", &cwd).expect("no refusal");

        assert_eq!(prompt.attachments.len(), 1);
        assert_eq!(prompt.attachments[0].token, "@\"two words.txt\"");
        assert!(prompt.sent.starts_with("read <file path=\"two words.txt\""));
        assert!(prompt.sent.ends_with("</file> now"));
    }

    #[test]
    fn a_file_that_is_not_text_is_refused_by_name() {
        let cwd = dir("binary");
        std::fs::write(cwd.join("blob.bin"), [0xff, 0xfe, 0x00, 0x01]).expect("the fixture file");

        let error = expand("@blob.bin", &cwd).expect_err("a binary is not a prompt");

        let message = format!("{error:#}");
        assert!(message.contains("@blob.bin"), "{message}");
        assert!(message.contains("not text"), "{message}");
    }

    #[test]
    fn too_much_in_one_prompt_is_refused_before_anything_is_sent() {
        let cwd = dir("toobig");
        let big = "x".repeat(MAX_INLINED_BYTES + 1);
        write(&cwd, "big.txt", &big);

        let error = expand("summarize @big.txt", &cwd).expect_err("that is not a prompt");

        let message = format!("{error:#}");
        assert!(message.contains("@big.txt"), "{message}");
        assert!(message.contains(&MAX_INLINED_BYTES.to_string()), "{message}");
    }

    #[test]
    fn one_directory_and_two_files_are_each_read_where_they_are() {
        let cwd = dir("two");
        write(&cwd, "one.txt", "1");
        write(&cwd, "two.txt", "2");

        let prompt = expand("@one.txt and @two.txt", &cwd).expect("no refusal");

        assert_eq!(prompt.attachments.len(), 2);
        assert!(
            prompt.sent.contains("\n1\n</file> and "),
            "{}",
            prompt.sent
        );
        assert!(prompt.sent.ends_with("\n2\n</file>"), "{}", prompt.sent);
    }
}
