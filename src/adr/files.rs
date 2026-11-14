//! A decision's two files in a repository: the decision doc under
//! `docs/adr/` (a header, then the Context File) and the rule file under
//! `.actual/rules/` (the same header, then the rules the checks enforce).

use std::path::{Path, PathBuf};

use crate::adr::header::{description_for, render_header, AdrHeader};
use crate::adr::names::{decision_doc_file_name, rule_file_name};
use crate::error::ActualError;
use crate::rules::types::AdrMeta;

/// Where decision docs live, relative to the repository root.
pub const DECISION_DIR: &str = "docs/adr";

/// Where rule files live, relative to the repository root.
pub const RULES_DIR: &str = ".actual/rules";

/// Decision and rule files larger than this are not read.
pub const MAX_FILE_SIZE: u64 = 1024 * 1024;

/// How deep under `docs/adr/` discovery looks, `docs/adr/drafts/` included.
const MAX_DEPTH: usize = 3;

/// A decision doc read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionFile {
    pub path: PathBuf,
    /// The header's fields, when the file opens with one.
    pub meta: Option<AdrMeta>,
    /// The Context File: everything after the header.
    pub body: String,
}

impl DecisionFile {
    /// The `adr_id` the header names, if any.
    pub fn adr_id(&self) -> Option<&str> {
        self.meta
            .as_ref()
            .and_then(|meta| meta.adr_id.as_deref())
            .filter(|id| !id.is_empty())
    }

    /// Whether the header marks the decision a draft.
    pub fn is_draft(&self) -> bool {
        self.meta
            .as_ref()
            .and_then(|meta| meta.status.as_deref())
            .is_some_and(|status| status.trim().eq_ignore_ascii_case("draft"))
    }
}

/// What discovery found under `docs/adr/`.
#[derive(Debug, Default)]
pub struct Discovered {
    /// Files whose header names an `adr_id`, sorted by path.
    pub decisions: Vec<DecisionFile>,
    /// Markdown files without such a header, such as an index or an ADR in
    /// another format.
    pub skipped: Vec<PathBuf>,
    /// Files that couldn't be read, with the reason.
    pub unreadable: Vec<(PathBuf, String)>,
}

/// `text` split at the end of its header: the header block (both `---`
/// lines, with the closing line's newline) and the body after it, less the
/// one blank line the sync writes between them. `None` when the text
/// doesn't open with a closed `---` block.
pub fn split_header(text: &str) -> Option<(&str, &str)> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim_end() != "---" {
        return None;
    }
    let mut offset = first.len();
    for line in lines {
        offset += line.len();
        if line.trim_end() == "---" {
            let (header, body) = text.split_at(offset);
            let body = body
                .strip_prefix("\r\n")
                .or_else(|| body.strip_prefix('\n'))
                .unwrap_or(body);
            return Some((header, body));
        }
    }
    None
}

/// The header's YAML as written: `metadata.actual` and `metadata.scope` nest.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct RawHeader {
    name: Option<String>,
    description: Option<String>,
    metadata: RawMetadata,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct RawMetadata {
    actual: RawActual,
    scope: RawScope,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct RawActual {
    adr_id: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct RawScope {
    root: Option<String>,
    paths: Vec<String>,
}

/// The fields of a header block, or `None` when its YAML doesn't read as a
/// header.
fn header_meta(header: &str) -> Option<AdrMeta> {
    let yaml: Vec<&str> = header
        .lines()
        .skip(1)
        .take_while(|line| line.trim_end() != "---")
        .collect();
    let raw: RawHeader = serde_yaml::from_str(&yaml.join("\n")).ok()?;
    Some(AdrMeta {
        name: raw.name,
        description: raw.description,
        adr_id: raw.metadata.actual.adr_id,
        status: raw.metadata.actual.status,
        root: raw.metadata.scope.root,
        paths: raw.metadata.scope.paths,
        ..AdrMeta::default()
    })
}

/// Read a decision doc, refusing one over [`MAX_FILE_SIZE`].
pub fn read_decision_file(path: &Path) -> Result<DecisionFile, ActualError> {
    let text = read_text(path)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let (meta, body) = match split_header(text) {
        Some((header, body)) => (header_meta(header), body),
        None => (None, text),
    };
    Ok(DecisionFile {
        path: path.to_path_buf(),
        meta,
        body: body.to_string(),
    })
}

fn read_text(path: &Path) -> Result<String, ActualError> {
    let named =
        |e: std::io::Error| std::io::Error::new(e.kind(), format!("{}: {e}", path.display()));
    let size = std::fs::metadata(path).map_err(named)?.len();
    if size > MAX_FILE_SIZE {
        return Err(ActualError::ConfigError(format!(
            "{} is {size} bytes, over the {MAX_FILE_SIZE}-byte limit for a decision file",
            path.display()
        )));
    }
    Ok(std::fs::read_to_string(path).map_err(named)?)
}

/// The decision docs under `<root>/docs/adr/` and its subdirectories.
pub fn discover(root: &Path) -> Discovered {
    let mut paths = Vec::new();
    collect_markdown(&root.join(DECISION_DIR), MAX_DEPTH, &mut paths);
    paths.sort();
    let mut found = Discovered::default();
    for path in paths {
        match read_decision_file(&path) {
            Ok(file) if file.adr_id().is_some() => found.decisions.push(file),
            Ok(_) => found.skipped.push(path),
            Err(e) => found.unreadable.push((path, e.to_string())),
        }
    }
    found
}

/// Push every `.md` file under `dir` onto `out`, `depth` levels down. A
/// directory that can't be listed contributes nothing; symlinks are not
/// followed.
fn collect_markdown(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for (path, kind) in entries
        .flatten()
        .filter_map(|entry| Some((entry.path(), entry.file_type().ok()?)))
    {
        if kind.is_dir() && depth > 1 {
            collect_markdown(&path, depth - 1, out);
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
}

/// The rule file under `<root>/.actual/rules/` whose header names `adr_id`.
pub fn find_rule_file(root: &Path, adr_id: &str) -> Option<PathBuf> {
    let mut paths = Vec::new();
    collect_markdown(&root.join(RULES_DIR), 1, &mut paths);
    paths.sort();
    paths.into_iter().find(|path| {
        read_decision_file(path)
            .ok()
            .is_some_and(|file| file.adr_id() == Some(adr_id))
    })
}

/// The repository root a decision doc sits in: the directory above its
/// `docs/adr/`, when the path runs through one. A relative path that starts
/// at `docs/adr/` gives `.`.
pub fn root_of(doc: &Path) -> Option<PathBuf> {
    doc.ancestors()
        .find(|dir| dir.ends_with(DECISION_DIR))
        .and_then(|adr_dir| adr_dir.parent()?.parent())
        .map(|root| {
            if root.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                root.to_path_buf()
            }
        })
}

/// `text` with its header replaced by `header`, the body kept byte for byte.
fn replace_header(text: &str, header: &str) -> String {
    let body = split_header(text).map_or(text, |(_, body)| body);
    format!("{header}\n{body}")
}

/// One file `rename_pair` moved.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Renamed {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// What `rename_pair` did: the files it moved, and anything it left alone
/// with the reason.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RenameOutcome {
    pub renamed: Vec<Renamed>,
    pub problems: Vec<String>,
}

/// After a push, move `doc` and its rule file to the names the sync writes
/// for decision `new_id` titled `title`, and point both headers at the new
/// id and the `status` it was saved with. The scope stays as it is. A rule
/// file that can't be found, or a target that already exists, is reported
/// and left alone.
pub fn rename_pair(
    root: &Path,
    doc: &DecisionFile,
    new_id: &str,
    title: &str,
    status: &str,
) -> RenameOutcome {
    let mut outcome = RenameOutcome::default();
    let meta = doc.meta.clone().unwrap_or_default();
    let header_for = |file_name: &str| AdrHeader {
        name: file_name.trim_end_matches(".md").to_string(),
        description: meta
            .description
            .clone()
            .unwrap_or_else(|| description_for(title)),
        adr_id: new_id.to_string(),
        status: status.to_string(),
        scope_root: meta.root.clone().unwrap_or_else(|| ".".to_string()),
        scope_paths: meta.paths.clone(),
    };

    let doc_name = decision_doc_file_name(new_id, title);
    move_with_header(
        &doc.path,
        &root.join(DECISION_DIR).join(&doc_name),
        &render_header(&header_for(&doc_name)),
        &mut outcome,
    );

    match doc.adr_id().and_then(|old| find_rule_file(root, old)) {
        Some(rule_path) => {
            let rule_name = rule_file_name(new_id, title);
            move_with_header(
                &rule_path,
                &root.join(RULES_DIR).join(&rule_name),
                &render_header(&header_for(&rule_name)),
                &mut outcome,
            );
        }
        None => outcome.problems.push(format!(
            "No rule file under {RULES_DIR}/ names this decision, so none was renamed."
        )),
    }
    outcome
}

/// Rewrite `from` as `to` with `header`, then remove `from`. Refuses to
/// overwrite another file.
fn move_with_header(from: &Path, to: &Path, header: &str, outcome: &mut RenameOutcome) {
    let result = (|| -> Result<(), ActualError> {
        let text = read_text(from)?;
        let text = replace_header(text.strip_prefix('\u{feff}').unwrap_or(&text), header);
        if from != to {
            if to.exists() {
                return Err(ActualError::ConfigError(format!(
                    "{} already exists",
                    to.display()
                )));
            }
            to.parent().map(std::fs::create_dir_all).transpose()?;
        }
        std::fs::write(to, text)?;
        if from != to {
            std::fs::remove_file(from)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => outcome.renamed.push(Renamed {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        }),
        Err(e) => outcome
            .problems
            .push(format!("{} was left as it is: {e}", from.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const OLD: &str = "0b6f2a1e-1111-2222-3333-444455556666";
    const NEW: &str = "c357aaaa-bbbb-4ccc-8ddd-eeeeffff0000";

    fn header(name: &str, id: &str, status: &str, paths: &[&str]) -> String {
        render_header(&AdrHeader {
            name: name.to_string(),
            description: "Use Postgres for storage".to_string(),
            adr_id: id.to_string(),
            status: status.to_string(),
            scope_root: ".".to_string(),
            scope_paths: paths.iter().map(|p| p.to_string()).collect(),
        })
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A pair as `actual adr new` writes it, under a local id.
    fn local_pair(root: &Path) -> (PathBuf, PathBuf) {
        let doc = root
            .join(DECISION_DIR)
            .join(format!("{OLD}-use-postgres-for-storage.md"));
        let rule = root
            .join(RULES_DIR)
            .join("cross-cutting-use-postgres-storage-0b6f.md");
        write(
            &doc,
            &format!(
                "{}\n# Use Postgres for storage\n\n## Context\n\n- Why.",
                header("doc", OLD, "accepted", &["src/**"])
            ),
        );
        write(
            &rule,
            &format!(
                "{}\n# Use Postgres for storage\n\n### Rules\n\n- **R-0B6F-001** MUST: Use it.\n",
                header("rule", OLD, "accepted", &["src/**"])
            ),
        );
        (doc, rule)
    }

    #[test]
    fn split_header_separates_the_block_and_drops_one_blank_line() {
        let (head, body) = split_header("---\nversion: 2\n---\n\n# T\n").unwrap();
        assert_eq!(head, "---\nversion: 2\n---\n");
        assert_eq!(body, "# T\n");
        let (_, crlf) = split_header("---\r\nversion: 2\r\n---\r\n\r\n# T").unwrap();
        assert_eq!(crlf, "# T");
        let (_, tight) = split_header("---\nversion: 2\n---\n# T").unwrap();
        assert_eq!(tight, "# T");
        assert_eq!(split_header("# T\n"), None);
        assert_eq!(split_header("---\nversion: 2\n"), None);
        assert_eq!(split_header(""), None);
    }

    #[test]
    fn read_decision_file_reads_header_and_body() {
        let tmp = tempdir().unwrap();
        let (doc, _) = local_pair(tmp.path());
        let file = read_decision_file(&doc).unwrap();
        assert_eq!(file.adr_id(), Some(OLD));
        assert!(!file.is_draft());
        assert!(file.body.starts_with("# Use Postgres for storage"));
        assert_eq!(file.meta.unwrap().paths, ["src/**"]);
    }

    #[test]
    fn read_decision_file_without_a_header_keeps_the_whole_text() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("plain.md");
        write(&path, "\u{feff}# Plain\n");
        let file = read_decision_file(&path).unwrap();
        assert_eq!(file.meta, None);
        assert_eq!(file.adr_id(), None);
        assert!(!file.is_draft());
        assert_eq!(file.body, "# Plain\n");
    }

    #[test]
    fn a_header_that_is_not_yaml_gives_no_fields() {
        assert_eq!(header_meta("---\n: [\n---\n"), None);
        let meta = header_meta("---\nversion: 2\n---\n").unwrap();
        assert_eq!(meta.adr_id, None);
    }

    #[test]
    fn read_decision_file_refuses_a_huge_or_missing_file() {
        let tmp = tempdir().unwrap();
        let big = tmp.path().join("big.md");
        write(&big, &"x".repeat(MAX_FILE_SIZE as usize + 1));
        assert!(matches!(
            read_decision_file(&big),
            Err(ActualError::ConfigError(m)) if m.contains("limit")
        ));
        assert!(matches!(
            read_decision_file(&tmp.path().join("missing.md")),
            Err(ActualError::IoError(_))
        ));
    }

    #[test]
    fn a_draft_header_and_an_empty_id_are_read_as_such() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("draft.md");
        write(&path, &format!("{}\n# T", header("d", "", "draft", &[])));
        let file = read_decision_file(&path).unwrap();
        assert!(file.is_draft());
        assert_eq!(file.adr_id(), None);
    }

    #[test]
    fn discover_sorts_decisions_from_the_rest() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (doc, _) = local_pair(root);
        write(&root.join(DECISION_DIR).join("README.md"), "# ADRs\n");
        write(&root.join(DECISION_DIR).join("notes.txt"), "not markdown");
        let draft = root.join(DECISION_DIR).join("drafts").join("next.md");
        write(
            &draft,
            &format!("{}\n# Next", header("next", NEW, "draft", &[])),
        );
        write(&root.join(DECISION_DIR).join("a/b/c/deep.md"), "# Too deep");
        let unreadable = root.join(DECISION_DIR).join("bad.md");
        write(&unreadable, "");
        std::fs::write(&unreadable, [0xff, 0xfe, 0x00]).unwrap();

        let found = discover(root);
        let ids: Vec<_> = found.decisions.iter().map(|d| d.path.clone()).collect();
        assert_eq!(ids, [doc, draft]);
        assert_eq!(found.skipped, [root.join(DECISION_DIR).join("README.md")]);
        assert_eq!(found.unreadable.len(), 1);
        assert_eq!(found.unreadable[0].0, unreadable);
    }

    #[test]
    fn discover_in_a_repository_with_no_decisions_finds_nothing() {
        let tmp = tempdir().unwrap();
        let found = discover(tmp.path());
        assert!(found.decisions.is_empty() && found.skipped.is_empty());
    }

    #[test]
    fn find_rule_file_matches_on_the_header_id() {
        let tmp = tempdir().unwrap();
        let (_, rule) = local_pair(tmp.path());
        write(
            &tmp.path().join(RULES_DIR).join("other.md"),
            &format!("{}\n# Other", header("other", NEW, "accepted", &[])),
        );
        assert_eq!(find_rule_file(tmp.path(), OLD), Some(rule));
        assert_eq!(find_rule_file(tmp.path(), "nope"), None);
    }

    #[test]
    fn root_of_finds_the_directory_above_docs_adr() {
        assert_eq!(
            root_of(Path::new("/repo/docs/adr/x.md")),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(
            root_of(Path::new("/repo/docs/adr/drafts/x.md")),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(root_of(Path::new("/repo/notes/x.md")), None);
        assert_eq!(
            root_of(Path::new("docs/adr/x.md")),
            Some(PathBuf::from("."))
        );
    }

    #[test]
    fn rename_pair_moves_both_files_and_rewrites_their_headers() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (doc, rule) = local_pair(root);
        let file = read_decision_file(&doc).unwrap();
        let outcome = rename_pair(root, &file, NEW, "Use Postgres for storage", "accepted");
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        assert_eq!(outcome.renamed.len(), 2);
        assert!(!doc.exists() && !rule.exists());

        let new_doc = root
            .join(DECISION_DIR)
            .join(decision_doc_file_name(NEW, "Use Postgres for storage"));
        let moved = read_decision_file(&new_doc).unwrap();
        assert_eq!(moved.adr_id(), Some(NEW));
        let meta = moved.meta.unwrap();
        assert_eq!(meta.status.as_deref(), Some("accepted"));
        assert_eq!(meta.paths, ["src/**"]);
        assert_eq!(
            meta.name.as_deref(),
            Some(decision_doc_file_name(NEW, "Use Postgres for storage").trim_end_matches(".md"))
        );
        assert_eq!(
            moved.body,
            "# Use Postgres for storage\n\n## Context\n\n- Why."
        );

        let new_rule = root
            .join(RULES_DIR)
            .join(rule_file_name(NEW, "Use Postgres for storage"));
        let rule_text = std::fs::read_to_string(&new_rule).unwrap();
        assert!(rule_text.contains("- **R-0B6F-001** MUST: Use it."));
        assert_eq!(find_rule_file(root, NEW), Some(new_rule));
    }

    #[test]
    fn rename_pair_gives_both_headers_the_status_the_decision_was_saved_with() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (doc, _) = local_pair(root);
        let file = read_decision_file(&doc).unwrap();
        assert_eq!(
            file.meta.as_ref().unwrap().status.as_deref(),
            Some("accepted")
        );
        let outcome = rename_pair(root, &file, NEW, "Use Postgres for storage", "draft");
        assert_eq!(outcome.renamed.len(), 2, "{:?}", outcome.problems);
        for renamed in &outcome.renamed {
            let moved = read_decision_file(&renamed.to).unwrap();
            assert_eq!(moved.meta.unwrap().status.as_deref(), Some("draft"));
        }
    }

    #[test]
    fn a_missing_decision_file_is_named_in_the_error() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("docs/adr/gone.md");
        let err = read_decision_file(&missing).err();
        assert!(
            matches!(&err, Some(ActualError::IoError(e)) if e.to_string().contains("gone.md")),
            "{err:?}"
        );
    }

    #[test]
    fn rename_pair_without_a_rule_file_moves_the_doc_and_says_so() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (doc, rule) = local_pair(root);
        std::fs::remove_file(&rule).unwrap();
        let file = read_decision_file(&doc).unwrap();
        let outcome = rename_pair(root, &file, NEW, "Use Postgres for storage", "accepted");
        assert_eq!(outcome.renamed.len(), 1);
        assert_eq!(outcome.problems.len(), 1);
        assert!(outcome.problems[0].contains("No rule file"));
    }

    #[test]
    fn rename_pair_of_a_file_without_a_header_writes_one() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let doc = root.join(DECISION_DIR).join("drafts").join("plain.md");
        write(&doc, "# Plain title\n\n## Context\n\n- Why.");
        let file = read_decision_file(&doc).unwrap();
        let outcome = rename_pair(root, &file, NEW, "Plain title", "accepted");
        assert_eq!(outcome.renamed.len(), 1);
        let moved = read_decision_file(&outcome.renamed[0].to).unwrap();
        assert_eq!(moved.adr_id(), Some(NEW));
        let meta = moved.meta.unwrap();
        assert_eq!(meta.status.as_deref(), Some("accepted"));
        assert_eq!(meta.root.as_deref(), Some("."));
        assert_eq!(moved.body, "# Plain title\n\n## Context\n\n- Why.");
    }

    #[test]
    fn rename_pair_never_overwrites_an_existing_file() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (doc, _) = local_pair(root);
        let taken = root
            .join(DECISION_DIR)
            .join(decision_doc_file_name(NEW, "Use Postgres for storage"));
        write(&taken, "someone else's");
        let file = read_decision_file(&doc).unwrap();
        let outcome = rename_pair(root, &file, NEW, "Use Postgres for storage", "accepted");
        assert!(doc.exists());
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "someone else's");
        assert!(
            outcome.problems[0].contains("already exists"),
            "{:?}",
            outcome.problems
        );
    }

    #[test]
    fn rename_in_place_rewrites_the_header_only() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let name = decision_doc_file_name(NEW, "Use Postgres for storage");
        let doc = root.join(DECISION_DIR).join(&name);
        write(
            &doc,
            &format!(
                "{}\n# Use Postgres for storage",
                header("x", OLD, "draft", &[])
            ),
        );
        let file = read_decision_file(&doc).unwrap();
        let outcome = rename_pair(root, &file, NEW, "Use Postgres for storage", "draft");
        assert_eq!(outcome.renamed[0].from, outcome.renamed[0].to);
        let moved = read_decision_file(&doc).unwrap();
        assert_eq!(moved.adr_id(), Some(NEW));
        assert!(moved.is_draft());
    }

    #[test]
    fn rename_drops_a_byte_order_mark_with_the_old_header() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (doc, _) = local_pair(root);
        let text = std::fs::read_to_string(&doc).unwrap();
        write(&doc, &format!("\u{feff}{text}"));
        let file = read_decision_file(&doc).unwrap();
        let outcome = rename_pair(root, &file, NEW, "Use Postgres for storage", "accepted");
        assert!(outcome.problems.is_empty(), "{:?}", outcome.problems);
        let new_doc = root
            .join(DECISION_DIR)
            .join(decision_doc_file_name(NEW, "Use Postgres for storage"));
        let moved = std::fs::read_to_string(&new_doc).unwrap();
        assert!(
            !moved.contains(OLD) && !moved.contains('\u{feff}'),
            "{moved}"
        );
    }

    #[test]
    fn rename_reports_a_file_that_vanished() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let (doc, _) = local_pair(root);
        let file = read_decision_file(&doc).unwrap();
        std::fs::remove_file(&doc).unwrap();
        let outcome = rename_pair(root, &file, NEW, "Use Postgres for storage", "accepted");
        assert!(
            outcome.problems[0].contains("left as it is"),
            "{:?}",
            outcome.problems
        );
    }
}
