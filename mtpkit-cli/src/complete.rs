//! Tab-completion for the interactive shell: command names, remote device
//! paths (folders and files, served from the shell's listing cache), and
//! local filesystem paths where a command takes a local file.

use std::cell::RefCell;
use std::rc::Rc;

use rustyline::Context;
use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;

use crate::shell::Shell;

const COMMANDS: &[&str] = &[
    "cd", "df", "dir", "exit", "get", "help", "info", "ls", "mkdir", "put", "pwd", "quit", "rm",
    "storages",
];

/// The rustyline helper wired into the interactive shell.
///
/// Shares the [`Shell`] with the main loop via `Rc<RefCell<…>>`: completion
/// borrows it between keystrokes (never while a command runs) and navigates
/// with save/restore so the user's location is untouched.
pub struct MtpHelper {
    shell: Rc<RefCell<Shell>>,
    files: FilenameCompleter,
}

impl MtpHelper {
    pub fn new(shell: Rc<RefCell<Shell>>) -> Self {
        Self {
            shell,
            files: FilenameCompleter::new(),
        }
    }

    /// Complete a remote path: resolve the directory part relative to the
    /// current location, then match the last component against its entries.
    fn complete_remote(&self, start: usize, word: &str, dirs_only: bool) -> (usize, Vec<Pair>) {
        let (dir_prefix, base) = match word.rsplit_once('/') {
            Some((dir, base)) => (format!("{dir}/"), base),
            None => (String::new(), word),
        };
        let Ok(mut shell) = self.shell.try_borrow_mut() else {
            return (start, Vec::new());
        };
        let saved = shell.save();
        let candidates = (|| -> anyhow::Result<Vec<(String, bool)>> {
            if !dir_prefix.is_empty() {
                let target = if dir_prefix == "/" {
                    "/"
                } else {
                    dir_prefix.trim_end_matches('/')
                };
                shell.change_dir(target)?;
            }
            shell.completion_candidates()
        })();
        shell.restore(saved);
        drop(shell);

        let Ok(mut entries) = candidates else {
            return (start, Vec::new());
        };
        if dirs_only {
            entries.retain(|(_, is_dir)| *is_dir);
        }
        let base_lower = base.to_lowercase();
        let mut pairs = Vec::new();
        for (name, is_dir) in entries {
            if !name.to_lowercase().starts_with(&base_lower) {
                continue;
            }
            let mut replacement = escape(&format!("{dir_prefix}{name}"));
            let mut display = name;
            if is_dir {
                replacement.push('/');
                display.push('/');
            }
            pairs.push(Pair {
                display,
                replacement,
            });
        }
        (start, pairs)
    }
}

impl Completer for MtpHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let (done, start, word) = scan_words(&line[..pos]);
        if done.is_empty() {
            let pairs = COMMANDS
                .iter()
                .filter(|c| c.starts_with(&word))
                .map(|c| Pair {
                    display: c.to_string(),
                    replacement: format!("{c} "),
                })
                .collect();
            return Ok((start, pairs));
        }
        let cmd = done[0].as_str();
        let arg_index = done.len(); // 1 = first argument, 2 = second, ...
        match (cmd, arg_index) {
            // Remote directories only: navigation targets and upload destinations.
            ("cd", 1) | ("mkdir", 1) | ("put", 2) => Ok(self.complete_remote(start, &word, true)),
            // Remote files and directories.
            ("ls", 1) | ("dir", 1) | ("rm", 1) | ("get", 1) => {
                Ok(self.complete_remote(start, &word, false))
            }
            // Local filesystem paths.
            ("put", 1) | ("get", 2) => self.files.complete(line, pos, ctx),
            _ => Ok((start, Vec::new())),
        }
    }
}

impl Hinter for MtpHelper {
    type Hint = String;
}

impl Highlighter for MtpHelper {}

impl Validator for MtpHelper {}

impl rustyline::Helper for MtpHelper {}

/// Split the line before the cursor into completed words plus the word being
/// typed, honoring quotes and backslash escapes. Returns (completed words,
/// byte offset where the current word starts, current word decoded).
fn scan_words(s: &str) -> (Vec<String>, usize, String) {
    let mut done = Vec::new();
    let mut current = String::new();
    let mut start = 0;
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None => match c {
                '\'' | '"' => {
                    if !in_token {
                        start = i;
                        in_token = true;
                    }
                    quote = Some(c);
                }
                '\\' => {
                    if !in_token {
                        start = i;
                        in_token = true;
                    }
                    if let Some((_, next)) = chars.next() {
                        current.push(next);
                    }
                }
                c if c.is_whitespace() => {
                    if in_token {
                        done.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                }
                c => {
                    if !in_token {
                        start = i;
                        in_token = true;
                    }
                    current.push(c);
                }
            },
        }
    }
    if in_token {
        (done, start, current)
    } else {
        (done, s.len(), String::new())
    }
}

/// Backslash-escape the characters the tokenizer treats specially.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, ' ' | '\'' | '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_words_empty_line() {
        assert_eq!(scan_words(""), (vec![], 0, String::new()));
        assert_eq!(scan_words("   "), (vec![], 3, String::new()));
    }

    #[test]
    fn scan_words_completing_first_word() {
        assert_eq!(scan_words("l"), (vec![], 0, "l".to_string()));
        assert_eq!(scan_words("ls"), (vec![], 0, "ls".to_string()));
    }

    #[test]
    fn scan_words_completing_an_argument() {
        let (done, start, word) = scan_words("cd Bo");
        assert_eq!(done, vec!["cd"]);
        assert_eq!(start, 3);
        assert_eq!(word, "Bo");
    }

    #[test]
    fn scan_words_fresh_argument_after_space() {
        let (done, start, word) = scan_words("cd ");
        assert_eq!(done, vec!["cd"]);
        assert_eq!(start, 3);
        assert_eq!(word, "");
    }

    #[test]
    fn scan_words_decodes_escapes_and_quotes() {
        let (done, start, word) = scan_words(r"put My\ Bo");
        assert_eq!(done, vec!["put"]);
        assert_eq!(start, 4);
        assert_eq!(word, "My Bo");

        let (done, start, word) = scan_words(r#"put "My Bo"#);
        assert_eq!(done, vec!["put"]);
        assert_eq!(start, 4);
        assert_eq!(word, "My Bo");
    }

    #[test]
    fn scan_words_second_argument() {
        let (done, start, word) = scan_words("put local.txt Bo");
        assert_eq!(done, vec!["put", "local.txt"]);
        assert_eq!(start, 14);
        assert_eq!(word, "Bo");
    }

    #[test]
    fn escape_protects_special_characters() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("My Books"), r"My\ Books");
        assert_eq!(escape(r"a\b"), r"a\\b");
        assert_eq!(escape("it's"), r"it\'s");
    }

    #[test]
    fn escaped_names_roundtrip_through_the_tokenizer() {
        for name in ["My Books", "it's here", r"back\slash", "plain"] {
            let escaped = escape(name);
            let tokens = crate::shell::tokenize(&format!("cd {escaped}")).unwrap();
            assert_eq!(tokens, vec!["cd".to_string(), name.to_string()]);
        }
    }
}
