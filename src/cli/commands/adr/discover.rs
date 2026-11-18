//! `actual adr discover`: the ADR files this repository keeps where ADR tools
//! put them, listed with no sign-in, and the ones outside docs/adr copied into
//! `docs/adr/imported/`. The run ends with `adr check`'s list of the files
//! under docs/adr that won't publish.

use std::fmt::Write as _;

use serde::Serialize;

use super::local::{render_cloud, render_wont_publish, run_check, CheckReport};
use super::{json_text, runtime, today};
use crate::adr::discover::{
    discover, import, Discovery, ImportState, Imports, COPIED_LIST, IMPORT_DIR,
};
use crate::cli::args::{AdrCheckArgs, AdrDiscoverArgs};
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};

/// What `--json` prints.
#[derive(Serialize)]
struct Output<'a> {
    #[serde(flatten)]
    found: &'a Discovery,
    imports: &'a Imports,
    wont_publish: &'a [super::local::WontPublish],
    adr_bot_copies: &'a [String],
    cloud: &'a super::local::CloudComparison,
}

pub(super) fn exec_discover(args: &AdrDiscoverArgs) -> Result<(), ActualError> {
    let root = args.root.clone().unwrap_or_else(resolve_cwd);
    let found = discover(&root);
    let imports = import(&root, &found, args.dry_run);
    let check = AdrCheckArgs {
        files: Vec::new(),
        no_cloud: args.no_cloud,
        repo: None,
        root: Some(root.clone()),
        api: args.api.clone(),
    };
    let report = runtime()?.block_on(run_check(&check, &root, &today()));
    if args.api.json {
        let output = Output {
            found: &found,
            imports: &imports,
            wont_publish: &report.wont_publish,
            adr_bot_copies: &report.adr_bot_copies,
            cloud: &report.cloud,
        };
        println!("{}", json_text(&output));
    } else {
        print!("{}", render(&found, &imports, &report));
    }
    let failed = imports.count(ImportState::Failed);
    match (&imports.error, failed) {
        (Some(error), _) => Err(ActualError::IoError(std::io::Error::other(error.clone()))),
        (None, 0) => Ok(()),
        (None, failed) => Err(ActualError::IoError(std::io::Error::other(format!(
            "{} of {} couldn't be copied",
            failed,
            count(imports.files.len(), "ADR file", "ADR files")
        )))),
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn render(found: &Discovery, imports: &Imports, report: &CheckReport) -> String {
    let mut out = String::new();
    if found.adrs.is_empty() {
        out.push_str(
            "No ADR files in the folders ADR tools use: docs/adr, doc/adr, docs/decisions, \
             docs/adrs, docs/architecture/decisions, architecture/decisions, adr, decisions, and \
             any that .adr-dir, .log4brains.yml or catalog-info.yaml names.\n",
        );
    } else {
        let read = found.folders.iter().filter(|f| f.exists).count();
        let _ = writeln!(
            out,
            "{} {} in {}",
            theme::heading("Found"),
            count(found.adrs.len(), "ADR file", "ADR files"),
            count(read, "folder", "folders")
        );
    }
    for folder in &found.folders {
        let by: Vec<&str> = folder.found_by.iter().map(|by| by.label()).collect();
        let _ = write!(out, "\n{}  ({})", folder.path, by.join(", "));
        if !folder.exists {
            out.push_str(": not found\n");
            continue;
        }
        out.push('\n');
        for adr in found.adrs.iter().filter(|adr| adr.folder == folder.path) {
            let title = adr.title.as_deref().map_or_else(
                || "(no title)".to_string(),
                |title| format!("\"{}\"", sanitize_server_text(title)),
            );
            let _ = write!(
                out,
                "  {}  {}  {}  {title}",
                adr.path,
                adr.layout.as_str(),
                adr.status
                    .as_deref()
                    .map_or("-".to_string(), sanitize_server_text)
            );
            if let Some(id) = &adr.adr_id {
                let _ = write!(out, "  id {}", sanitize_server_text(id));
            }
            out.push('\n');
        }
    }
    out.push_str(&render_imports(imports));
    if !found.other_files.is_empty() {
        let _ = writeln!(
            out,
            "\nNot ADRs ({}): {}",
            found.other_files.len(),
            found.other_files.join(", ")
        );
    }
    for file in &found.unreadable {
        let _ = writeln!(
            out,
            "{} {} couldn't be read: {}",
            theme::warning("warning:"),
            file.path,
            file.reason
        );
    }
    for note in &found.notes {
        let _ = writeln!(out, "{} {note}", theme::warning("note:"));
    }
    out.push_str(&render_wont_publish(report));
    if !found.adrs.is_empty() {
        out.push_str(&render_cloud(&report.cloud));
    }
    out
}

/// The copies made, or a dry run's, then what wasn't copied and why.
fn render_imports(imports: &Imports) -> String {
    let mut out = String::new();
    for (state, heading) in [
        (ImportState::Copied, "Copied"),
        (ImportState::WouldCopy, "Would copy"),
    ] {
        let n = imports.count(state);
        if n == 0 {
            continue;
        }
        let _ = writeln!(
            out,
            "\n{} {} into {IMPORT_DIR}/{}:",
            theme::heading(heading),
            count(n, "file", "files"),
            if state == ImportState::WouldCopy {
                " (dry run)"
            } else {
                ""
            }
        );
        for file in imports.files.iter().filter(|file| file.state == state) {
            let _ = writeln!(out, "  {} -> {}", file.source, file.copy);
        }
    }
    let before = imports.count(ImportState::CopiedBefore);
    if before > 0 {
        let _ = writeln!(
            out,
            "\nCopied before, so left alone: {} that {IMPORT_DIR}/{COPIED_LIST} lists.",
            count(before, "file", "files")
        );
    }
    for file in &imports.files {
        match file.state {
            ImportState::Exists => {
                let _ = writeln!(
                    out,
                    "{} {} isn't copied: {} is already there.",
                    theme::warning("note:"),
                    file.source,
                    file.copy
                );
            }
            ImportState::Failed => {
                let _ = writeln!(
                    out,
                    "{} {} couldn't be copied to {}: {}",
                    theme::warning("warning:"),
                    file.source,
                    file.copy,
                    file.reason.as_deref().unwrap_or_default()
                );
            }
            _ => {}
        }
    }
    if let Some(error) = &imports.error {
        let _ = writeln!(out, "{} {error}", theme::warning("warning:"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adr::discover::{AdrFile, Folder, FoundBy, Import, Layout, Unreadable};
    use crate::cli::args::AdrApiOptions;
    use crate::testutil::{EnvGuard, ENV_MUTEX};
    use tempfile::tempdir;

    fn adr(path: &str, layout: Layout, status: Option<&str>, id: Option<&str>) -> AdrFile {
        AdrFile {
            path: path.to_string(),
            folder: path.rsplit_once('/').unwrap().0.to_string(),
            layout,
            title: (layout != Layout::Other).then(|| "Use \u{1b}[31mPostgres".to_string()),
            status: status.map(str::to_string),
            adr_id: id.map(str::to_string),
        }
    }

    fn imported(source: &str, state: ImportState, reason: Option<&str>) -> Import {
        Import {
            source: source.to_string(),
            copy: format!("{IMPORT_DIR}/x/{source}"),
            state,
            reason: reason.map(str::to_string),
        }
    }

    /// `check`'s report on a repository with nothing under docs/adr/.
    fn empty_report() -> CheckReport {
        let tmp = tempdir().unwrap();
        let args = AdrCheckArgs {
            files: Vec::new(),
            no_cloud: true,
            repo: None,
            root: None,
            api: AdrApiOptions::default(),
        };
        runtime()
            .unwrap()
            .block_on(run_check(&args, tmp.path(), "2026-11-18"))
    }

    fn discover_args(root: &std::path::Path, dry_run: bool, json: bool) -> AdrDiscoverArgs {
        AdrDiscoverArgs {
            root: Some(root.to_path_buf()),
            dry_run,
            no_cloud: true,
            api: AdrApiOptions {
                api_url: None,
                json,
            },
        }
    }

    const NYGARD: &str = "# 1. Record\n\n## Status\n\nAccepted\n\n## Context\n\n## Decision\n";

    #[test]
    fn render_lists_each_folder_its_files_and_what_was_copied() {
        let found = Discovery {
            folders: vec![
                Folder {
                    path: "docs/adr".to_string(),
                    found_by: vec![FoundBy::Actual, FoundBy::Log4brains],
                    exists: true,
                },
                Folder {
                    path: "doc/arch".to_string(),
                    found_by: vec![FoundBy::AdrTools],
                    exists: false,
                },
                Folder {
                    path: "decisions".to_string(),
                    found_by: vec![FoundBy::Common],
                    exists: true,
                },
            ],
            adrs: vec![
                adr(
                    "docs/adr/a.md",
                    Layout::Actual,
                    Some("accepted"),
                    Some("id-1"),
                ),
                adr("decisions/0001-b.md", Layout::Other, None, None),
            ],
            other_files: vec!["docs/adr/README.md".to_string()],
            unreadable: vec![Unreadable {
                path: "decisions/0002-c.md".to_string(),
                reason: "too big".to_string(),
            }],
            notes: vec!["catalog-info.yaml names a URL.".to_string()],
        };
        let imports = Imports {
            files: vec![
                imported("decisions/0001-b.md", ImportState::Copied, None),
                imported("decisions/0003-d.md", ImportState::CopiedBefore, None),
                imported("decisions/0004-e.md", ImportState::CopiedBefore, None),
                imported("decisions/0005-f.md", ImportState::Exists, None),
                imported("decisions/0006-g.md", ImportState::Failed, Some("denied")),
            ],
            error: Some("the list couldn't be updated".to_string()),
        };
        let text = render(&found, &imports, &empty_report());
        assert!(text.contains("Found 2 ADR files in 2 folders"), "{text}");
        assert!(
            text.contains("\ndocs/adr  (Actual, log4brains)\n"),
            "{text}"
        );
        assert!(
            text.contains("  docs/adr/a.md  actual  accepted  \"Use [31mPostgres\"  id id-1\n"),
            "{text}"
        );
        assert!(
            text.contains("\ndoc/arch  (adr-tools): not found\n"),
            "{text}"
        );
        assert!(
            text.contains("  decisions/0001-b.md  other  -  (no title)\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "\nCopied 1 file into docs/adr/imported/:\n  decisions/0001-b.md -> docs/adr/imported/x/decisions/0001-b.md\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "Copied before, so left alone: 2 files that docs/adr/imported/.copied lists."
            ),
            "{text}"
        );
        assert!(
            text.contains("decisions/0005-f.md isn't copied: docs/adr/imported/x/decisions/0005-f.md is already there."),
            "{text}"
        );
        assert!(
            text.contains("decisions/0006-g.md couldn't be copied to docs/adr/imported/x/decisions/0006-g.md: denied"),
            "{text}"
        );
        assert!(
            text.contains("warning: the list couldn't be updated"),
            "{text}"
        );
        assert!(text.contains("Not ADRs (1): docs/adr/README.md"), "{text}");
        assert!(
            text.contains("decisions/0002-c.md couldn't be read: too big"),
            "{text}"
        );
        assert!(text.contains("catalog-info.yaml names a URL."), "{text}");
        assert!(text.contains("Skipped with --no-cloud."), "{text}");
    }

    #[test]
    fn render_says_when_nothing_was_found_or_what_a_dry_run_would_copy() {
        let found = Discovery {
            adrs: vec![adr("adr/0001-x.md", Layout::Nygard, None, None)],
            folders: vec![Folder {
                path: "adr".to_string(),
                found_by: vec![FoundBy::Common],
                exists: true,
            }],
            ..Discovery::default()
        };
        let dry = Imports {
            files: vec![imported("adr/0001-x.md", ImportState::WouldCopy, None)],
            error: None,
        };
        let text = render(&found, &dry, &empty_report());
        assert!(text.starts_with("Found 1 ADR file in 1 folder\n"), "{text}");
        assert!(
            text.contains("Would copy 1 file into docs/adr/imported/ (dry run):"),
            "{text}"
        );
        assert!(!text.contains("Copied before"), "{text}");
        let text = render(&Discovery::default(), &Imports::default(), &empty_report());
        assert!(
            text.starts_with("No ADR files in the folders ADR tools use"),
            "{text}"
        );
        assert!(!text.contains("Skipped with --no-cloud."), "{text}");
    }

    #[test]
    fn exec_discover_copies_then_lists_what_wont_publish() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("doc/adr")).unwrap();
        std::fs::write(root.join("doc/adr/0001-record.md"), NYGARD).unwrap();
        for json in [false, true] {
            assert!(exec_discover(&discover_args(root, true, json)).is_ok());
        }
        assert!(!root.join(IMPORT_DIR).exists(), "a dry run writes nothing");
        assert!(exec_discover(&discover_args(root, false, false)).is_ok());
        let copy = root.join(IMPORT_DIR).join("doc-adr/0001-record.md");
        assert_eq!(std::fs::read_to_string(copy).unwrap(), NYGARD);
        assert!(exec_discover(&discover_args(root, false, true)).is_ok());
    }

    #[test]
    fn exec_discover_fails_when_a_copy_or_the_list_fails() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("doc/adr")).unwrap();
        std::fs::write(root.join("doc/adr/0001-record.md"), NYGARD).unwrap();
        std::fs::create_dir_all(root.join(IMPORT_DIR)).unwrap();
        // A file where the copy's folder would go.
        std::fs::write(root.join(IMPORT_DIR).join("doc-adr"), "").unwrap();
        let err = exec_discover(&discover_args(root, false, false)).unwrap_err();
        assert!(
            matches!(&err, ActualError::IoError(e) if e.to_string() == "1 of 1 ADR file couldn't be copied"),
            "{err:?}"
        );
        std::fs::remove_file(root.join(IMPORT_DIR).join("doc-adr")).unwrap();
        std::fs::create_dir_all(root.join(IMPORT_DIR).join(COPIED_LIST)).unwrap();
        let err = exec_discover(&discover_args(root, false, false)).unwrap_err();
        assert!(
            matches!(&err, ActualError::IoError(e) if e.to_string().starts_with("docs/adr/imported/.copied couldn't be read")),
            "{err:?}"
        );
    }

    #[test]
    fn exec_discover_compares_with_actual_ai_unless_told_not_to() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let _g1 = EnvGuard::remove("ACTUAL_CONFIG");
        let config = tempdir().unwrap();
        let _g2 = EnvGuard::set("ACTUAL_CONFIG_DIR", config.path().to_str().unwrap());
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("docs/adr")).unwrap();
        std::fs::write(tmp.path().join("docs/adr/0001-record.md"), NYGARD).unwrap();
        let mut args = discover_args(tmp.path(), false, false);
        args.no_cloud = false;
        // Signed out, so the list is made from this repository's files alone.
        assert!(exec_discover(&args).is_ok());
    }
}
