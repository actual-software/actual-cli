//! The files a Codex tool call names, for `rules brief --codex-hook`.
//!
//! # Why this exists
//!
//! Claude Code's `Read`, `Edit` and `Write` tools carry a `file_path`. Codex
//! has none of them: it reads files through its shell tool (reported to hooks
//! as `Bash`, with the command line in `tool_input.command`) and changes them
//! with `apply_patch` (the patch text in `tool_input.command`). So the paths
//! have to be read out of those two strings.
//!
//! **Shell reads.** Only a command that is plainly a read is briefed: every
//! pipeline stage starts with a program from [`READ_ONLY_PROGRAMS`], nothing is
//! written through a redirection, and there is no command substitution,
//! heredoc or background job. Anything else returns no paths, because a brief
//! after an arbitrary command would be noise at best. The words are split the
//! way a shell splits them, honouring quotes, so the `|` inside
//! `rg -n "getUser|db\."` is part of a pattern rather than a pipe; Phase 0
//! captured exactly that command from a real Codex session.
//!
//! The candidate paths are the words that look like a file with an extension,
//! as the actual-cli Claude Code mod decides it (`PATH_TOKEN` in
//! `hooks/register.js`, ported here): a pattern argument such as `foo.bar`
//! passes that test, so the caller keeps only candidates that exist.
//!
//! **Patches.** `*** Add File:`, `*** Update File:` and `*** Move to:` headers
//! name the files a patch writes. Codex has been seen to write these as
//! absolute paths and as relative ones, so both are accepted. A deleted file
//! is not briefed.

use std::path::{Path, PathBuf};

/// How many files one shell command may brief, as in the Claude Code mod: a
/// command reading six files would otherwise inject six briefs at once.
pub const MAX_READ_PATHS: usize = 2;

/// How many files one patch may brief. A patch touching more is usually a
/// mechanical change across a codebase, where a brief per file is noise.
pub const MAX_PATCH_PATHS: usize = 3;

/// Programs whose only effect is to print what they read. `sed` is here
/// because `sed -n` is how agents page through files; `-i` is refused
/// separately.
const READ_ONLY_PROGRAMS: &[&str] = &[
    "bat", "cat", "cut", "diff", "egrep", "fgrep", "grep", "head", "jq", "less", "more", "nl",
    "rg", "sed", "tail", "tr", "uniq", "wc",
];

/// One shell word or operator.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    /// `|`, `||`, `&&` or `;`: the end of a pipeline stage.
    Separator,
    /// `<`: the next word is a file read on stdin.
    InputRedirect,
    /// Anything that rules a command out: an output redirection to a file,
    /// a heredoc, a background `&`, or command substitution.
    Unsafe,
}

/// Split `command` into words and operators, honouring single quotes,
/// double quotes and backslash escapes. Output redirections to `/dev/null`
/// or to another descriptor (`2>&1`) are harmless and dropped.
fn tokenize(command: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars().peekable();

    fn flush(tokens: &mut Vec<Token>, word: &mut String, in_word: &mut bool) {
        if *in_word {
            tokens.push(Token::Word(std::mem::take(word)));
            *in_word = false;
        }
    }

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    word.push(q);
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => {
                            if let Some(&next) = chars.peek() {
                                if matches!(next, '"' | '\\' | '$' | '`') {
                                    chars.next();
                                    word.push(next);
                                    continue;
                                }
                            }
                            word.push('\\');
                        }
                        '$' if chars.peek() == Some(&'(') => {
                            tokens.push(Token::Unsafe);
                            word.push(q);
                        }
                        '`' => {
                            tokens.push(Token::Unsafe);
                            word.push(q);
                        }
                        _ => word.push(q),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            '$' if chars.peek() == Some(&'(') => {
                tokens.push(Token::Unsafe);
                in_word = true;
                word.push(c);
            }
            '`' => {
                tokens.push(Token::Unsafe);
                in_word = true;
                word.push(c);
            }
            c if c.is_whitespace() => flush(&mut tokens, &mut word, &mut in_word),
            '|' | ';' => {
                flush(&mut tokens, &mut word, &mut in_word);
                if c == '|' && chars.peek() == Some(&'|') {
                    chars.next();
                }
                tokens.push(Token::Separator);
            }
            '&' => {
                flush(&mut tokens, &mut word, &mut in_word);
                if chars.peek() == Some(&'&') {
                    chars.next();
                    tokens.push(Token::Separator);
                } else {
                    tokens.push(Token::Unsafe);
                }
            }
            '<' => {
                flush(&mut tokens, &mut word, &mut in_word);
                if chars.peek() == Some(&'<') {
                    tokens.push(Token::Unsafe);
                    chars.next();
                } else {
                    tokens.push(Token::InputRedirect);
                }
            }
            '>' => {
                // A descriptor number written right before the `>` (`2>`) is
                // part of the redirection, not an argument.
                if in_word && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                flush(&mut tokens, &mut word, &mut in_word);
                if chars.peek() == Some(&'>') {
                    chars.next();
                }
                while chars.peek().is_some_and(|w| w.is_whitespace()) {
                    chars.next();
                }
                let mut target = String::new();
                while let Some(&t) = chars.peek() {
                    if t.is_whitespace() || matches!(t, '|' | ';' | '<' | '>') {
                        break;
                    }
                    if t == '&' && !target.is_empty() {
                        break;
                    }
                    target.push(t);
                    chars.next();
                }
                let harmless = target == "/dev/null"
                    || (target.starts_with('&')
                        && target[1..].chars().all(|d| d.is_ascii_digit())
                        && target.len() > 1);
                if !harmless {
                    tokens.push(Token::Unsafe);
                }
            }
            _ => {
                in_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut tokens, &mut word, &mut in_word);
    tokens
}

/// Whether `word` looks like a file with an extension: the Claude Code mod's
/// `PATH_TOKEN`, `^(/|~/)?[\w.][\w./-]*\.[A-Za-z][A-Za-z0-9]{0,5}$`. No `*`,
/// so a glob is not a file; the extension starts with a letter, so a version
/// number is not one either.
fn looks_like_file(word: &str) -> bool {
    let body = word
        .strip_prefix("~/")
        .or_else(|| word.strip_prefix('/'))
        .unwrap_or(word);
    let mut chars = body.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    if !(is_word(first) || first == '.') {
        return false;
    }
    if !body
        .chars()
        .all(|c| is_word(c) || matches!(c, '.' | '/' | '-'))
    {
        return false;
    }
    let Some((stem, ext)) = body.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || ext.is_empty() || ext.len() > 6 {
        return false;
    }
    let mut ext_chars = ext.chars();
    ext_chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && ext_chars.all(|c| c.is_ascii_alphanumeric())
}

/// The program a stage runs, without its directory: `/usr/bin/cat` is `cat`.
fn program_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Join `word` onto `base`, expanding a leading `~/` against `home`.
fn resolve_word(word: &str, base: &Path, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(rest) = word.strip_prefix("~/") {
        return home.map(|home| home.join(rest));
    }
    let path = Path::new(word);
    Some(if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    })
}

/// The files a read-only shell command reads, resolved against `cwd` (and
/// any leading `cd` chain), keeping at most [`MAX_READ_PATHS`] that exist.
/// Empty for any command that is not plainly a read.
pub fn shell_read_paths(command: &str, cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let tokens = tokenize(command);
    if tokens.contains(&Token::Unsafe) {
        return Vec::new();
    }

    let mut stages: Vec<Vec<Token>> = vec![Vec::new()];
    for token in tokens {
        if token == Token::Separator {
            stages.push(Vec::new());
        } else if let Some(stage) = stages.last_mut() {
            stage.push(token);
        }
    }
    stages.retain(|stage| !stage.is_empty());

    // A leading chain of `cd`s moves the base for everything after it; a `cd`
    // anywhere later makes the base ambiguous, so nothing is briefed.
    let mut base = cwd.to_path_buf();
    let mut stages = stages.into_iter().peekable();
    while let Some(stage) = stages.next_if(|stage| stage.first() == Some(&Token::Word("cd".into())))
    {
        let Some(Token::Word(target)) = stage.get(1) else {
            return Vec::new();
        };
        match resolve_word(target, &base, home) {
            Some(next) => base = next,
            None => return Vec::new(),
        }
    }
    let stages: Vec<Vec<Token>> = stages.collect();

    let mut candidates: Vec<PathBuf> = Vec::new();
    for stage in &stages {
        let Some(Token::Word(program)) = stage.first() else {
            return Vec::new();
        };
        let program = program_name(program);
        if !READ_ONLY_PROGRAMS.contains(&program) {
            return Vec::new();
        }
        let words = stage[1..].iter().filter_map(|t| match t {
            Token::Word(w) => Some(w.as_str()),
            _ => None,
        });
        if program == "sed"
            && words
                .clone()
                .any(|w| w.starts_with("-i") || w == "--in-place")
        {
            return Vec::new();
        }
        for word in words {
            if word.starts_with('-') || !looks_like_file(word) {
                continue;
            }
            if let Some(path) = resolve_word(word, &base, home) {
                if !candidates.contains(&path) {
                    candidates.push(path);
                }
            }
        }
    }
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .take(MAX_READ_PATHS)
        .collect()
}

/// The files an `apply_patch` patch adds, updates or moves to, resolved
/// against `cwd`, at most [`MAX_PATCH_PATHS`].
pub fn patch_paths(patch: &str, cwd: &Path) -> Vec<PathBuf> {
    const HEADERS: &[&str] = &["*** Add File:", "*** Update File:", "*** Move to:"];
    let mut paths: Vec<PathBuf> = Vec::new();
    for line in patch.lines() {
        let Some(named) = HEADERS
            .iter()
            .find_map(|header| line.strip_prefix(header))
            .map(str::trim)
            .filter(|named| !named.is_empty())
        else {
            continue;
        };
        let path = Path::new(named);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        };
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths.truncate(MAX_PATCH_PATHS);
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "src/app.ts",
            "src/db.ts",
            "src/web/page.tsx",
            "docs/a.md",
            "README.md",
        ] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        dir
    }

    fn reads(command: &str, cwd: &Path) -> Vec<String> {
        shell_read_paths(command, cwd, Some(Path::new("/home/me")))
            .into_iter()
            .map(|p| p.strip_prefix(cwd).unwrap_or(&p).display().to_string())
            .collect()
    }

    #[test]
    fn test_plain_reads() {
        let r = repo();
        assert_eq!(reads("cat src/app.ts", r.path()), ["src/app.ts"]);
        assert_eq!(reads("sed -n '1,40p' src/db.ts", r.path()), ["src/db.ts"]);
        assert_eq!(reads("head -n 5 README.md | nl", r.path()), ["README.md"]);
        assert_eq!(reads("wc -l < docs/a.md", r.path()), ["docs/a.md"]);
        assert_eq!(reads("/bin/cat src/app.ts", r.path()), ["src/app.ts"]);
    }

    /// The compound read Phase 0 captured from a real Codex session: the `|`
    /// inside the quoted pattern is not a pipe, and the pattern is not a file.
    #[test]
    fn test_the_captured_compound_read() {
        let r = repo();
        let command =
            r#"sed -n '1,240p' src/app.ts && rg -n "getUser|user.*(find|query)|database|db\." src"#;
        assert_eq!(reads(command, r.path()), ["src/app.ts"]);
    }

    #[test]
    fn test_caps_and_dedups() {
        let r = repo();
        assert_eq!(
            reads("cat src/app.ts src/app.ts src/db.ts README.md", r.path()),
            ["src/app.ts", "src/db.ts"]
        );
    }

    /// A word that looks like a file but is not one (a pattern, a missing
    /// file) is dropped, and does not use up the cap.
    #[test]
    fn test_keeps_only_files_that_exist() {
        let r = repo();
        assert_eq!(
            reads("grep -n user.find missing.ts src/db.ts", r.path()),
            ["src/db.ts"]
        );
        assert!(reads("cat src", r.path()).is_empty());
    }

    #[test]
    fn test_leading_cd_moves_the_base() {
        let r = repo();
        assert_eq!(reads("cd src && cat app.ts", r.path()), ["src/app.ts"]);
        assert_eq!(
            reads("cd src; cd web && cat page.tsx", r.path()),
            ["src/web/page.tsx"]
        );
        let absolute = format!("cd {} && cat src/db.ts", r.path().display());
        assert_eq!(reads(&absolute, Path::new("/elsewhere")).len(), 1);
    }

    #[test]
    fn test_ambiguous_or_broken_cd_reads_nothing() {
        let r = repo();
        assert!(reads("cat src/app.ts && cd src && cat db.ts", r.path()).is_empty());
        assert!(reads("cd && cat src/app.ts", r.path()).is_empty());
        assert!(shell_read_paths("cd ~/x && cat a.ts", r.path(), None).is_empty());
    }

    #[test]
    fn test_home_and_absolute_paths() {
        let r = repo();
        let home = r.path();
        let found = shell_read_paths("cat ~/src/app.ts", Path::new("/"), Some(home));
        assert_eq!(found, [home.join("src/app.ts")]);
        assert!(shell_read_paths("cat ~/src/app.ts", Path::new("/"), None).is_empty());
        let absolute = format!("cat {}", r.path().join("src/db.ts").display());
        assert_eq!(shell_read_paths(&absolute, Path::new("/"), None).len(), 1);
    }

    /// Anything that is not plainly a read briefs nothing.
    #[test]
    fn test_refuses_commands_that_write_or_run_code() {
        let r = repo();
        for command in [
            "cat src/app.ts > out.ts",
            "cat src/app.ts >> out.ts",
            "cat src/app.ts | tee copy.ts",
            "sed -i 's/a/b/' src/app.ts",
            "sed --in-place 's/a/b/' src/app.ts",
            "python3 src/app.ts",
            "cat $(ls src/app.ts)",
            "cat `ls src/app.ts`",
            "cat \"$(echo src/app.ts)\"",
            "cat \"`echo src/app.ts`\"",
            "cat src/app.ts &",
            "cat <<EOF\nsrc/app.ts\nEOF",
            "< src/app.ts cat",
            "",
            "|",
        ] {
            assert!(reads(command, r.path()).is_empty(), "{command}");
        }
    }

    #[test]
    fn test_harmless_redirections_are_allowed() {
        let r = repo();
        assert_eq!(
            reads("cat src/app.ts 2>/dev/null", r.path()),
            ["src/app.ts"]
        );
        assert_eq!(
            reads("cat src/app.ts 2>&1 | head", r.path()),
            ["src/app.ts"]
        );
        assert_eq!(reads("cat src/app.ts >/dev/null", r.path()), ["src/app.ts"]);
        assert_eq!(
            reads("rg x src/app.ts || cat src/db.ts", r.path()),
            ["src/app.ts", "src/db.ts"]
        );
        assert_eq!(
            reads("cat src/app.ts 2>/dev/null&& cat src/db.ts", r.path()),
            ["src/app.ts", "src/db.ts"]
        );
        assert!(reads("cat src/app.ts 2>err.log", r.path()).is_empty());
        assert!(reads("cat src/app.ts >&", r.path()).is_empty());
    }

    #[test]
    fn test_quotes_and_escapes() {
        let r = repo();
        assert_eq!(reads(r#"cat "src/app.ts""#, r.path()), ["src/app.ts"]);
        assert_eq!(reads(r#"cat src/\app.ts"#, r.path()), ["src/app.ts"]);
        assert_eq!(
            reads(r#"grep "a\"b\\c\$d\x" src/db.ts"#, r.path()),
            ["src/db.ts"]
        );
        assert!(reads("cat trailing\\", r.path()).is_empty());
        // An unclosed quote ending in a backslash keeps the backslash, so the
        // word names no file.
        assert!(reads("cat \"src/app.ts\\", r.path()).is_empty());
    }

    #[test]
    fn test_looks_like_file() {
        for yes in [
            "a.ts",
            "src/a.d.ts",
            "/abs/x.json",
            "~/x.md",
            ".env.local",
            "a-b_c.yaml",
        ] {
            assert!(looks_like_file(yes), "{yes}");
        }
        for no in [
            "",
            "src",
            "src/*.ts",
            "9.0.3",
            "a.",
            ".",
            "~/",
            "x.toolongext",
            "a,b.ts",
            "-n.ts",
            "/.x",
        ] {
            assert!(!looks_like_file(no), "{no}");
        }
        assert!(looks_like_file(".x.y"));
    }

    #[test]
    fn test_patch_paths() {
        let cwd = Path::new("/repo");
        let patch = "*** Begin Patch\n*** Update File: /repo/src/app.ts\n@@\n-a\n+b\n\
                     *** Add File: src/new.ts\n+x\n*** Delete File: src/old.ts\n\
                     *** Update File: src/move.ts\n*** Move to: src/moved.ts\n\
                     *** Update File: src/app.ts\n*** End Patch";
        assert_eq!(
            patch_paths(patch, cwd),
            [
                PathBuf::from("/repo/src/app.ts"),
                PathBuf::from("/repo/src/new.ts"),
                PathBuf::from("/repo/src/move.ts"),
            ]
        );
        assert!(patch_paths("*** Update File:   \n", cwd).is_empty());
        assert!(patch_paths("not a patch", cwd).is_empty());
    }
}
