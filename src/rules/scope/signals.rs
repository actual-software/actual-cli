//! Extraction of applicability signal from a parsed rule document.
//!
//! # Design
//!
//! A rule file says where it applies in three places, none of which the
//! status-quo filename scan reads:
//!
//! * the **`### Verify` block**, whose `grep`/`find`/`test` operands name
//!   concrete paths (`apps/actual/lib/oauth/`) — the strongest signal, because
//!   it is the one the rule's own author had to make executable;
//! * the **prose scope sentence** under the title, which names the domain in
//!   words ("OAuth token issuance and verification");
//! * the **aspect slug**, the middle segment of the filename.
//!
//! Everything here is a pure function of text. No I/O, no globals, no ordering
//! dependence — so the extractors are exercised entirely from string fixtures,
//! and a caller holding rule text from elsewhere can use them directly.
//!
//! The shell tokenizer is deliberately partial. It is not a shell: it splits on
//! unquoted whitespace and pipeline separators, honours quotes and backslash
//! escapes, and stops there. A verify block is a hint about where a rule
//! applies, so mis-reading one line costs a little recall on one document,
//! never correctness.

use std::collections::BTreeSet;

/// Words carrying no discriminating power in any corpus.
///
/// Deliberately short. Terms that are ubiquitous *in a particular rule set* —
/// `rules`, `always`, `active`, and the `cross-cutting` filename prefix that
/// motivated this task — are neutralized by inverse document frequency at index
/// time, which needs no list and adapts to the corpus at hand. This list only
/// covers closed-class English words, where the judgement is not corpus-specific.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "any", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have",
    "in", "into", "is", "it", "its", "of", "on", "or", "that", "the", "their", "them", "then",
    "there", "these", "they", "this", "to", "was", "were", "when", "where", "which", "while",
    "who", "will", "with", "within",
];

/// True for a token that carries no signal on its own.
fn is_stopword(token: &str) -> bool {
    STOPWORDS.binary_search(&token).is_ok()
}

/// Fold an English plural to its singular so `tokens` and `token` match.
///
/// A three-rule s-stemmer, not a real stemmer. It is applied to both documents
/// and queries, so an over-aggressive fold costs nothing as long as it is
/// consistent: both sides land on the same string.
fn singularize(token: &str) -> String {
    if token.len() > 4 && token.ends_with("ies") {
        return format!("{}y", &token[..token.len() - 3]);
    }
    if token.len() > 4
        && (token.ends_with("sses") || token.ends_with("shes") || token.ends_with("ches"))
    {
        return token[..token.len() - 2].to_string();
    }
    if token.len() > 3
        && token.ends_with('s')
        && !token.ends_with("ss")
        && !token.ends_with("us")
        && !token.ends_with("is")
    {
        return token[..token.len() - 1].to_string();
    }
    token.to_string()
}

/// Split an identifier on case and separator boundaries.
///
/// `signInWithPassword` becomes `sign in with password`, `mcp-gateway` becomes
/// `mcp gateway`, `OAuthToken` becomes `oauth token`. An acronym run followed by
/// a word (`JWKSKey`) splits before the final capital, which is the usual
/// convention and the one that keeps `JWKS` intact.
fn split_identifier(raw: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = raw.chars().collect();
    let mut start = 0usize;
    for i in 1..chars.len() {
        let prev = chars[i - 1];
        let cur = chars[i];
        let next = chars.get(i + 1).copied();
        let boundary = (prev.is_lowercase() && cur.is_uppercase())
            || (prev.is_ascii_digit() != cur.is_ascii_digit())
            || (prev.is_uppercase()
                && cur.is_uppercase()
                && next.is_some_and(|n| n.is_lowercase()));
        if boundary {
            out.push(chars[start..i].iter().collect());
            start = i;
        }
    }
    out.push(chars[start..].iter().collect());
}

/// Normalize free text into matchable terms.
///
/// Lowercased, split on non-alphanumeric boundaries and then on case
/// boundaries, singularized, with stopwords, single characters and bare numbers
/// dropped. The same function runs over documents and over queries, which is
/// the only thing that makes the two comparable.
pub fn terms(text: &str) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        split_identifier(raw, &mut pieces);
    }
    pieces
        .into_iter()
        .map(|piece| singularize(&piece.to_lowercase()))
        .filter(|term| {
            term.chars().count() > 1
                && !term.chars().all(|c| c.is_ascii_digit())
                && !is_stopword(term)
        })
        .collect()
}

/// A path pattern a rule file points at, normalized to a repository-relative
/// glob.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathGlob {
    /// The normalized pattern, e.g. `apps/actual/lib/oauth/**`.
    pub pattern: String,
}

impl PathGlob {
    /// The leading path segments before the first wildcard.
    ///
    /// `apps/actual/lib/oauth/**` yields `["apps", "actual", "lib", "oauth"]`.
    /// This is what makes containment comparable in both directions: a plan
    /// naming a file below the prefix matches, and so does a plan naming an
    /// ancestor of it.
    pub fn literal_segments(&self) -> Vec<&str> {
        self.pattern
            .split('/')
            .take_while(|seg| !seg.contains(['*', '?', '[', '{']))
            .filter(|seg| !seg.is_empty() && *seg != ".")
            .collect()
    }
}

/// Everything one document says about where it applies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentSignals {
    /// Path globs from the `### Verify` operands, and from any path written
    /// into the prose scope sentence.
    pub globs: Vec<PathGlob>,
    /// File-extension globs from `--include=` filters, e.g. `*.ts`.
    pub extensions: Vec<String>,
    /// Terms from the prose scope sentence.
    pub scope_terms: Vec<String>,
    /// Terms from the aspect slug — the filename minus its topic prefix and
    /// content hash.
    pub slug_terms: Vec<String>,
    /// Terms from the document title.
    pub title_terms: Vec<String>,
    /// Terms from the path segments named in the verify block, so a plan that
    /// says "OAuth token signing" reaches a rule that greps `lib/oauth/`
    /// without naming the directory.
    pub path_terms: Vec<String>,
}

/// True for a token that looks like a repository path rather than a flag, a
/// URL, a regex, or a bare word.
///
/// Requires either a `/` or a known source-file extension, which is what keeps
/// grep patterns such as `signInWithPassword` and `@dataclass` out.
fn is_path_like(token: &str) -> bool {
    if token.is_empty() || token.starts_with('-') || token.contains("://") {
        return false;
    }
    // A regex alternation or anchor is a pattern, not a path.
    if token.contains(['|', '^', '$', '\\', '+', '"', '\'', '=', '<', '>', '`']) {
        return false;
    }
    if token.starts_with('@') {
        return false;
    }
    let has_slash = token.contains('/');
    let has_source_extension = std::path::Path::new(token)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(is_source_extension);
    (has_slash || has_source_extension)
        && token
            .chars()
            .all(|c| c.is_alphanumeric() || "._-/*?()[]{}~".contains(c))
}

/// Extensions worth treating as evidence that a token names a file.
fn is_source_extension(ext: &str) -> bool {
    const EXTS: &[&str] = &[
        "c", "cfg", "cjs", "cpp", "cs", "css", "go", "graphql", "h", "hcl", "hpp", "html", "ini",
        "java", "js", "json", "jsx", "kt", "lock", "md", "mjs", "php", "prisma", "proto", "py",
        "rb", "rs", "scss", "sh", "sql", "swift", "tf", "tfvars", "toml", "ts", "tsx", "vue",
        "yaml", "yml",
    ];
    EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Normalize a path operand into a glob.
///
/// Strips shell escaping and a `./` prefix, drops a trailing slash, and turns a
/// bare directory into a recursive glob. A token that already carries a
/// wildcard is left alone.
fn normalize_path(token: &str) -> Option<PathGlob> {
    let unescaped = token.replace('\\', "");
    let trimmed = unescaped.trim_start_matches("./").trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return None;
    }
    let looks_like_file = std::path::Path::new(trimmed)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(is_source_extension);
    let has_wildcard = trimmed.contains(['*', '?']);
    let pattern = if looks_like_file || has_wildcard {
        trimmed.to_string()
    } else {
        // A bare directory operand governs everything beneath it.
        format!("{trimmed}/**")
    };
    Some(PathGlob { pattern })
}

/// Split one verify-block line into commands, honouring quotes and escapes.
///
/// Pipeline and list separators (`|`, `;`, `&&`, `||`) start a new command, so
/// `grep -r "x" dir/ | wc -l` does not hand `wc` the directory. A comment
/// (`#`) outside quotes ends the line.
fn shell_commands(line: &str) -> Vec<Vec<String>> {
    let mut commands: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut token = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();

    let flush_token = |token: &mut String, current: &mut Vec<String>| {
        if !token.is_empty() {
            current.push(std::mem::take(token));
        }
    };

    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    token.push(c);
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '\\' => {
                    // Keep the escape so `normalize_path` can strip it, and so
                    // `is_path_like` still rejects a regex escape.
                    token.push('\\');
                    if let Some(next) = chars.next() {
                        token.push(next);
                    }
                }
                '#' if token.is_empty() && current.is_empty() => break,
                '|' | ';' | '&' => {
                    flush_token(&mut token, &mut current);
                    if !current.is_empty() {
                        commands.push(std::mem::take(&mut current));
                    }
                    // Consume a doubled separator (`&&`, `||`) as one.
                    if chars.peek() == Some(&c) {
                        chars.next();
                    }
                }
                '>' | '<' => {
                    flush_token(&mut token, &mut current);
                    // Everything after a redirection is a stream, not an operand.
                    if !current.is_empty() {
                        commands.push(std::mem::take(&mut current));
                    }
                    for next in chars.by_ref() {
                        if next == ';' || next == '|' {
                            break;
                        }
                    }
                }
                c if c.is_whitespace() => flush_token(&mut token, &mut current),
                c => token.push(c),
            },
        }
    }
    flush_token(&mut token, &mut current);
    if !current.is_empty() {
        commands.push(current);
    }
    commands
}

/// `grep` options that consume the following argument, so the argument is never
/// mistaken for a path operand.
const GREP_OPTIONS_WITH_VALUE: &[&str] = &[
    "-e",
    "-f",
    "-m",
    "-A",
    "-B",
    "-C",
    "-d",
    "--regexp",
    "--file",
    "--max-count",
    "--after-context",
    "--before-context",
    "--context",
    "--include",
    "--exclude",
    "--include-dir",
    "--exclude-dir",
    "--binary-files",
    "--devices",
    "--directories",
    "--color",
    "--colour",
    "--label",
];

/// Pull path globs and extension filters out of one command's tokens.
///
/// The command name decides how operands are read:
///
/// * `grep`/`rg` — the first bare operand is the *pattern* and is skipped,
///   unless the pattern was already given with `-e`. `--include=GLOB` is an
///   extension filter, not a path.
/// * `find` — operands before the first `-predicate` are paths; `-name` and
///   `-path` take globs.
/// * `test` / `[` — the operand after a file test is a path.
/// * anything else — every path-shaped token is taken, which is how the
///   `python -c "... open('apps/x/y.py') ..."` lines in the corpus are read.
fn extract_from_command(
    tokens: &[String],
    globs: &mut Vec<PathGlob>,
    extensions: &mut Vec<String>,
) {
    let Some(name) = tokens.first().map(|t| command_name(t)) else {
        return;
    };
    match name {
        "grep" | "egrep" | "fgrep" | "rg" | "ag" => {
            extract_from_grep(&tokens[1..], globs, extensions);
        }
        "find" | "fd" => extract_from_find(&tokens[1..], globs, extensions),
        "test" | "[" | "[[" => {
            let mut expect_path = false;
            for token in &tokens[1..] {
                if matches!(
                    token.as_str(),
                    "-f" | "-d" | "-e" | "-s" | "-r" | "-x" | "-L"
                ) {
                    expect_path = true;
                    continue;
                }
                if expect_path {
                    push_path(token, globs);
                    expect_path = false;
                }
            }
        }
        _ => {
            for token in &tokens[1..] {
                collect_option_value(token, extensions);
                push_path(token, globs);
            }
        }
    }
}

/// The command name with any leading path stripped: `/usr/bin/grep` is `grep`.
fn command_name(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

fn extract_from_grep(args: &[String], globs: &mut Vec<PathGlob>, extensions: &mut Vec<String>) {
    let mut pattern_seen = false;
    let mut skip_next = false;
    for token in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if token.starts_with('-') && token.len() > 1 {
            collect_option_value(token, extensions);
            if GREP_OPTIONS_WITH_VALUE.contains(&token.as_str()) {
                // `-e PATTERN` supplies the pattern, so the first bare operand
                // after it is already a path.
                if matches!(token.as_str(), "-e" | "--regexp" | "-f" | "--file") {
                    pattern_seen = true;
                }
                skip_next = true;
            }
            continue;
        }
        if !pattern_seen {
            pattern_seen = true;
            continue;
        }
        push_path(token, globs);
    }
}

fn extract_from_find(args: &[String], globs: &mut Vec<PathGlob>, extensions: &mut Vec<String>) {
    let mut in_predicates = false;
    let mut expect_glob = false;
    for token in args {
        if expect_glob {
            expect_glob = false;
            record_extension(token, extensions);
            if token.contains('/') {
                push_path(token, globs);
            }
            continue;
        }
        if token.starts_with('-') {
            in_predicates = true;
            if matches!(
                token.as_str(),
                "-name" | "-iname" | "-path" | "-ipath" | "-wholename"
            ) {
                expect_glob = true;
            }
            continue;
        }
        if !in_predicates {
            push_path(token, globs);
        }
    }
}

/// Record `--include=*.ts` style filters as extension signal.
fn collect_option_value(token: &str, extensions: &mut Vec<String>) {
    if let Some((flag, value)) = token.split_once('=') {
        if flag.starts_with("--include") || flag == "--glob" || flag == "-g" || flag == "--type" {
            record_extension(value, extensions);
        }
    }
}

/// Keep an extension glob in its canonical `*.ext` form.
fn record_extension(value: &str, extensions: &mut Vec<String>) {
    let cleaned = value.trim_matches(['"', '\'']).trim_start_matches("*.");
    let cleaned = cleaned.trim_start_matches('.');
    if !cleaned.is_empty() && is_source_extension(cleaned) {
        let glob = format!("*.{}", cleaned.to_ascii_lowercase());
        if !extensions.contains(&glob) {
            extensions.push(glob);
        }
    }
}

fn push_path(token: &str, globs: &mut Vec<PathGlob>) {
    if is_path_like(token) {
        record_path(token, globs);
        return;
    }
    // A path can sit inside a larger token: the corpus verifies some rules with
    // `python -c "... open('services/x/registry.py') ..."`, where the operand is
    // a whole expression. Split on the delimiters a path never contains and
    // retry the pieces. Nothing is lost by trying: a regex fragment still fails
    // `is_path_like` after the split.
    for piece in token.split(['\'', '"', '(', ')', '[', ']', '{', '}', ',']) {
        if is_path_like(piece) {
            record_path(piece, globs);
        }
    }
}

fn record_path(token: &str, globs: &mut Vec<PathGlob>) {
    if let Some(glob) = normalize_path(token) {
        if !globs.contains(&glob) {
            globs.push(glob);
        }
    }
}

/// Path globs and extension filters named anywhere in a verify block body.
pub fn globs_from_verify(body: &str) -> (Vec<PathGlob>, Vec<String>) {
    let mut globs = Vec::new();
    let mut extensions = Vec::new();
    for line in body.lines() {
        for command in shell_commands(line) {
            extract_from_command(&command, &mut globs, &mut extensions);
        }
    }
    (globs, extensions)
}

/// Path globs written into prose — the scope sentence often names a directory
/// outright, in backticks or bare.
pub fn globs_from_prose(text: &str) -> Vec<PathGlob> {
    let mut globs = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || c == '`' || c == ',') {
        let token = raw
            .trim_end_matches(['.', ')', ':', ';'])
            .trim_start_matches('(');
        // Prose paths must carry a slash: a bare `auth.ts` in a sentence is
        // usually an example, and a bare word is never a path.
        if token.contains('/') && is_path_like(token) {
            push_path(token, &mut globs);
        }
    }
    globs
}

/// The aspect segments of a rule filename.
///
/// A rule file is named `<topic>-<aspect>-<hash>`. The trailing hash is dropped
/// here because it is provably not a word; the topic prefix is *not* dropped,
/// because which segments are topic is corpus-specific — a prefix shared by
/// every file is neutralized by its inverse document frequency at index time,
/// which needs no rule and no threshold.
pub fn slug_terms(slug: &str) -> Vec<String> {
    let mut segments: Vec<&str> = slug.split('-').collect();
    if segments.last().is_some_and(|last| is_content_hash(last)) {
        segments.pop();
    }
    terms(&segments.join(" "))
}

/// True for a short all-hex segment, the shape of a content hash.
fn is_content_hash(segment: &str) -> bool {
    (4..=12).contains(&segment.len())
        && segment.chars().all(|c| c.is_ascii_hexdigit())
        && segment.chars().any(|c| c.is_ascii_alphabetic())
}

/// Extract every applicability signal from one parsed document.
pub fn extract(doc: &crate::rules::RuleDocument) -> DocumentSignals {
    let mut globs: Vec<PathGlob> = Vec::new();
    let mut extensions: Vec<String> = Vec::new();

    for block in &doc.verify {
        let (block_globs, block_extensions) = globs_from_verify(&block.body);
        for glob in block_globs {
            if !globs.contains(&glob) {
                globs.push(glob);
            }
        }
        for ext in block_extensions {
            if !extensions.contains(&ext) {
                extensions.push(ext);
            }
        }
    }

    let scope_text = doc.scope.as_deref().unwrap_or_default();
    for glob in globs_from_prose(scope_text) {
        if !globs.contains(&glob) {
            globs.push(glob);
        }
    }

    // Path segments become terms so a plan phrased in words ("OAuth token
    // signing") reaches a rule that only ever names `lib/oauth/`.
    let mut path_terms: BTreeSet<String> = BTreeSet::new();
    for glob in &globs {
        for segment in glob.literal_segments() {
            path_terms.extend(terms(segment));
        }
    }

    DocumentSignals {
        globs,
        extensions,
        scope_terms: terms(scope_text),
        slug_terms: doc.slug().map(slug_terms).unwrap_or_default(),
        title_terms: terms(doc.title.as_deref().unwrap_or_default()),
        path_terms: path_terms.into_iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;

    /// Helper: the pattern strings from a verify body, for terse assertions.
    fn patterns(body: &str) -> Vec<String> {
        globs_from_verify(body)
            .0
            .into_iter()
            .map(|g| g.pattern)
            .collect()
    }

    // ── terms ────────────────────────────────────────────────────────────

    #[test]
    fn test_terms_lowercases_splits_and_drops_stopwords() {
        assert_eq!(
            terms("Token signing IS in the module"),
            vec!["token", "signing", "module"]
        );
    }

    #[test]
    fn test_terms_splits_camel_case_and_separators() {
        // `with` is a stopword, so the split survives but the word does not.
        assert_eq!(terms("signInWithPassword"), vec!["sign", "password"]);
        assert_eq!(terms("mcp-gateway"), vec!["mcp", "gateway"]);
        assert_eq!(terms("mcp_gateway"), vec!["mcp", "gateway"]);
    }

    /// An acronym run followed by a word splits before the final capital, so
    /// the acronym survives whole.
    #[test]
    fn test_terms_splits_acronym_runs_before_a_trailing_word() {
        assert_eq!(terms("JWKSKeyRotation"), vec!["jwk", "key", "rotation"]);
    }

    #[test]
    fn test_terms_drops_single_characters_and_bare_numbers() {
        assert_eq!(terms("a b 42 ok"), vec!["ok"]);
        assert_eq!(terms(""), Vec::<String>::new());
    }

    /// Singular and plural must land on the same term, or a plan saying
    /// "tokens" never reaches a rule saying "token".
    #[test]
    fn test_terms_folds_plurals_onto_the_singular() {
        assert_eq!(terms("tokens"), terms("token"));
        assert_eq!(terms("policies"), terms("policy"));
        assert_eq!(terms("classes"), terms("class"));
    }

    /// The fold must not chew words that merely end in `s`.
    #[test]
    fn test_singularize_leaves_non_plurals_alone() {
        for word in ["access", "status", "analysis", "bus", "class"] {
            assert_eq!(singularize(word), word, "mangled: {word}");
        }
    }

    #[test]
    fn test_stopwords_are_sorted_for_binary_search() {
        let mut sorted = STOPWORDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, STOPWORDS, "STOPWORDS must stay sorted");
        assert!(is_stopword("the"));
        assert!(!is_stopword("token"));
    }

    // ── path shape ───────────────────────────────────────────────────────

    #[test]
    fn test_is_path_like_accepts_paths_and_source_files() {
        assert!(is_path_like("apps/actual/lib/oauth/"));
        assert!(is_path_like("registry.py"));
        assert!(is_path_like("packages/*/src"));
    }

    /// The discriminating case: a grep pattern must never be read as a path.
    #[test]
    fn test_is_path_like_rejects_patterns_flags_and_urls() {
        for token in [
            "signInWithPassword",
            "@dataclass",
            "jwt\\.sign.*RS256",
            "-r",
            "--include=*.ts",
            "https://example.com/x",
            "class.*Input(BaseModel)|other",
            "process.env.OAUTH_TOKEN_ISSUER",
            "",
        ] {
            assert!(!is_path_like(token), "accepted a non-path: {token}");
        }
    }

    #[test]
    fn test_normalize_path_turns_a_directory_into_a_recursive_glob() {
        assert_eq!(
            normalize_path("apps/actual/lib/oauth/").unwrap().pattern,
            "apps/actual/lib/oauth/**"
        );
        assert_eq!(
            normalize_path("./backend/db").unwrap().pattern,
            "backend/db/**"
        );
    }

    #[test]
    fn test_normalize_path_leaves_files_and_wildcards_alone() {
        assert_eq!(
            normalize_path("lib/auth.ts").unwrap().pattern,
            "lib/auth.ts"
        );
        assert_eq!(
            normalize_path("packages/*/src").unwrap().pattern,
            "packages/*/src"
        );
    }

    #[test]
    fn test_normalize_path_strips_shell_escapes() {
        assert_eq!(
            normalize_path("web/app/\\(public\\)").unwrap().pattern,
            "web/app/(public)/**"
        );
    }

    #[test]
    fn test_normalize_path_rejects_degenerate_operands() {
        assert!(normalize_path(".").is_none());
        assert!(normalize_path("..").is_none());
        assert!(normalize_path("/").is_none());
    }

    #[test]
    fn test_literal_segments_stop_at_the_first_wildcard() {
        let glob = PathGlob {
            pattern: "apps/actual/lib/oauth/**".to_string(),
        };
        assert_eq!(
            glob.literal_segments(),
            vec!["apps", "actual", "lib", "oauth"]
        );
        let wild = PathGlob {
            pattern: "packages/*/src".to_string(),
        };
        assert_eq!(wild.literal_segments(), vec!["packages"]);
    }

    // ── shell tokenizing ─────────────────────────────────────────────────

    #[test]
    fn test_shell_commands_honours_quotes_and_whitespace() {
        assert_eq!(
            shell_commands(r#"grep -r "a b" dir/"#),
            vec![vec!["grep", "-r", "a b", "dir/"]]
        );
    }

    /// A pipeline must not hand the next command the previous one's operands.
    #[test]
    fn test_shell_commands_splits_on_pipelines_and_lists() {
        assert_eq!(
            shell_commands("grep -r x dir/ | wc -l"),
            vec![vec!["grep", "-r", "x", "dir/"], vec!["wc", "-l"]]
        );
        assert_eq!(
            shell_commands("test -d a && echo ok"),
            vec![vec!["test", "-d", "a"], vec!["echo", "ok"]]
        );
    }

    #[test]
    fn test_shell_commands_ignores_a_leading_comment() {
        assert!(shell_commands("# Verify RS256 usage").is_empty());
    }

    #[test]
    fn test_shell_commands_stops_at_a_redirection() {
        assert_eq!(
            shell_commands("grep -r x dir/ > out.txt"),
            vec![vec!["grep", "-r", "x", "dir/"]]
        );
    }

    // ── verify extraction ────────────────────────────────────────────────

    /// The load-bearing case: grep's first bare operand is the pattern, and
    /// only what follows is a path.
    #[test]
    fn test_globs_from_verify_skips_the_grep_pattern() {
        assert_eq!(
            patterns(
                r#"grep -r "jwt\.sign.*RS256" apps/actual/lib/oauth/ apps/actual/lib/github/"#
            ),
            vec!["apps/actual/lib/oauth/**", "apps/actual/lib/github/**"]
        );
    }

    /// With `-e`, the pattern is already supplied, so the first bare operand
    /// is a path rather than a second pattern.
    #[test]
    fn test_globs_from_verify_handles_an_explicit_pattern_flag() {
        assert_eq!(
            patterns("grep -r -e RS256 services/auth/"),
            vec!["services/auth/**"]
        );
    }

    #[test]
    fn test_globs_from_verify_reads_include_filters_as_extensions() {
        let (globs, extensions) =
            globs_from_verify(r#"grep -r "x" web/lib --include="*.ts" --include=*.tsx"#);
        assert_eq!(
            globs.iter().map(|g| &g.pattern).collect::<Vec<_>>(),
            vec!["web/lib/**"]
        );
        assert_eq!(extensions, vec!["*.ts", "*.tsx"]);
    }

    /// An option that consumes its argument must not let that argument be read
    /// as a path.
    #[test]
    fn test_globs_from_verify_skips_option_arguments() {
        assert_eq!(
            patterns("grep -r --exclude-dir node_modules/ pat src/"),
            vec!["src/**"]
        );
    }

    #[test]
    fn test_globs_from_verify_reads_find_paths_and_name_globs() {
        let (globs, extensions) = globs_from_verify("find infra/terraform -name '*.tf'");
        assert_eq!(globs[0].pattern, "infra/terraform/**");
        assert_eq!(extensions, vec!["*.tf"]);
    }

    #[test]
    fn test_globs_from_verify_reads_file_tests() {
        assert_eq!(
            patterns("test -f .terraform.lock.hcl"),
            vec![".terraform.lock.hcl"]
        );
        assert_eq!(
            patterns("test -d backend/db/ && echo ok"),
            vec!["backend/db/**"]
        );
    }

    /// Unrecognized commands fall back to taking every path-shaped token,
    /// which is how the corpus's `python -c "... open('x/y.py') ..."` lines are
    /// read.
    #[test]
    fn test_globs_from_verify_falls_back_for_unknown_commands() {
        assert_eq!(
            patterns("python -c \"open('services/gateway/routing/registry.py').read()\""),
            vec!["services/gateway/routing/registry.py"]
        );
    }

    #[test]
    fn test_globs_from_verify_deduplicates_across_lines() {
        assert_eq!(patterns("grep -r a src/\ngrep -r b src/"), vec!["src/**"]);
    }

    #[test]
    fn test_globs_from_verify_on_an_empty_body() {
        assert!(patterns("").is_empty());
        assert!(patterns("echo hello").is_empty());
    }

    // ── prose extraction ─────────────────────────────────────────────────

    #[test]
    fn test_globs_from_prose_reads_a_directory_in_a_sentence() {
        let globs = globs_from_prose(
            "These rules are ALWAYS ACTIVE for activities in `backend/workers/activities/`, and their models.",
        );
        assert_eq!(
            globs.iter().map(|g| &g.pattern).collect::<Vec<_>>(),
            vec!["backend/workers/activities/**"]
        );
    }

    /// Prose paths must carry a slash: a bare filename in a sentence is an
    /// example, not a scope.
    #[test]
    fn test_globs_from_prose_ignores_bare_words_and_filenames() {
        assert!(globs_from_prose("These rules apply to auth.ts and tokens.").is_empty());
        assert!(globs_from_prose("no paths at all here").is_empty());
    }

    #[test]
    fn test_globs_from_prose_trims_trailing_punctuation() {
        let globs = globs_from_prose("code in services/auth/oauth/.");
        assert_eq!(globs[0].pattern, "services/auth/oauth/**");
    }

    // ── slug ─────────────────────────────────────────────────────────────

    #[test]
    fn test_slug_terms_drops_the_trailing_content_hash() {
        assert_eq!(
            slug_terms("cross-cutting-access-tokens-include-e410"),
            vec!["cross", "cutting", "access", "token", "include"]
        );
    }

    /// The topic prefix is deliberately *kept*: which segments are topic is
    /// corpus-specific, and inverse document frequency neutralizes a
    /// ubiquitous one at index time without a rule here.
    #[test]
    fn test_slug_terms_keeps_the_topic_prefix() {
        assert!(slug_terms("cross-cutting-x-ab12").contains(&"cross".to_string()));
    }

    #[test]
    fn test_is_content_hash_requires_a_short_hex_run_with_a_letter() {
        assert!(is_content_hash("e410"));
        assert!(is_content_hash("0aee"));
        // All digits is a version or a number, not a hash.
        assert!(!is_content_hash("2024"));
        assert!(!is_content_hash("tokens"));
        assert!(!is_content_hash("ab"));
    }

    #[test]
    fn test_slug_terms_without_a_hash_segment() {
        assert_eq!(slug_terms("oauth-tokens"), vec!["oauth", "token"]);
    }

    // ── extract ──────────────────────────────────────────────────────────

    const DOC: &str = r#"# Adopt RS256 Signing: Access Tokens

These rules are ALWAYS ACTIVE for OAuth token issuance in `services/auth/oauth/`.

### Rules

- **R-A-001** MUST: sign with RS256.

### Verify

```bash
grep -r "jwt.sign" services/auth/jwks/ --include="*.ts"
```
"#;

    #[test]
    fn test_extract_collects_every_signal_from_a_document() {
        let doc = crate::rules::parse_rule_document(
            Path::new("/repo/.actual/rules/cross-cutting-access-tokens-e410.md"),
            DOC,
        )
        .unwrap();
        let signals = extract(&doc);

        // Verify operands and the prose path both land in `globs`.
        assert!(signals
            .globs
            .iter()
            .any(|g| g.pattern == "services/auth/jwks/**"));
        assert!(signals
            .globs
            .iter()
            .any(|g| g.pattern == "services/auth/oauth/**"));
        assert_eq!(signals.extensions, vec!["*.ts"]);
        assert!(signals.scope_terms.contains(&"oauth".to_string()));
        assert!(signals.title_terms.contains(&"rs".to_string()));
        assert!(signals.slug_terms.contains(&"token".to_string()));
        // Path segments become terms so a worded plan reaches a path-only rule.
        assert!(signals.path_terms.contains(&"oauth".to_string()));
        assert!(signals.path_terms.contains(&"jwk".to_string()));
    }

    #[test]
    fn test_extract_on_a_document_with_no_verify_or_scope() {
        let doc = crate::rules::parse_rule_document(
            Path::new("/repo/.actual/rules/bare.md"),
            "# T\n\n### Rules\n\n- **R-A-001** MUST: x.\n",
        )
        .unwrap();
        let signals = extract(&doc);
        assert!(signals.globs.is_empty());
        assert!(signals.extensions.is_empty());
        assert!(signals.scope_terms.is_empty());
        assert!(signals.path_terms.is_empty());
        assert_eq!(signals.slug_terms, vec!["bare"]);
        assert_eq!(signals, signals.clone());
    }
}
