//! `actual adr discover`: the ADR files this repository keeps where ADR tools
//! put them, listed with no sign-in. Nothing is written or sent.

use std::fmt::Write as _;

use super::json_text;
use crate::adr::discover::{discover, Discovery, Layout};
use crate::adr::files::DECISION_DIR;
use crate::cli::args::AdrDiscoverArgs;
use crate::cli::commands::sync::resolve_cwd;
use crate::cli::ui::theme;
use crate::error::{sanitize_server_text, ActualError};

pub(super) fn exec_discover(args: &AdrDiscoverArgs) -> Result<(), ActualError> {
    let root = args.root.clone().unwrap_or_else(resolve_cwd);
    let found = discover(&root);
    if args.json {
        println!("{}", json_text(&found));
    } else {
        print!("{}", render(&found));
    }
    Ok(())
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn render(found: &Discovery) -> String {
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
    if !found.adrs.is_empty() {
        let actual = found
            .adrs
            .iter()
            .filter(|adr| adr.layout == Layout::Actual)
            .count();
        let _ = writeln!(
            out,
            "\nIn Actual's format: {actual}. `actual adr create --all` sends the ones under \
             {DECISION_DIR}/ that Actual AI doesn't have yet.\nIn other layouts: {}. `actual adr \
             create` sends only files in Actual's format.",
            found.adrs.len() - actual
        );
    }
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
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adr::discover::{AdrFile, Folder, FoundBy, Unreadable};
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

    #[test]
    fn render_lists_each_folder_its_files_and_what_to_send() {
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
        let text = render(&found);
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
        assert!(text.contains("In Actual's format: 1."), "{text}");
        assert!(text.contains("In other layouts: 1."), "{text}");
        assert!(text.contains("Not ADRs (1): docs/adr/README.md"), "{text}");
        assert!(
            text.contains("decisions/0002-c.md couldn't be read: too big"),
            "{text}"
        );
        assert!(text.contains("catalog-info.yaml names a URL."), "{text}");
    }

    #[test]
    fn render_says_when_nothing_was_found() {
        let found = Discovery {
            adrs: vec![adr("adr/0001-x.md", Layout::Nygard, None, None)],
            folders: vec![Folder {
                path: "adr".to_string(),
                found_by: vec![FoundBy::Common],
                exists: true,
            }],
            ..Discovery::default()
        };
        assert!(render(&found).starts_with("Found 1 ADR file in 1 folder\n"));
        let text = render(&Discovery::default());
        assert!(
            text.starts_with("No ADR files in the folders ADR tools use"),
            "{text}"
        );
        assert!(!text.contains("In Actual's format"), "{text}");
    }

    #[test]
    fn exec_discover_prints_text_or_json() {
        let tmp = tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("doc/adr")).unwrap();
        std::fs::write(
            tmp.path().join("doc/adr/0001-record.md"),
            "# 1. Record\n\n## Status\n\nAccepted\n\n## Context\n\n## Decision\n",
        )
        .unwrap();
        for json in [false, true] {
            let args = AdrDiscoverArgs {
                root: Some(tmp.path().to_path_buf()),
                json,
            };
            assert!(exec_discover(&args).is_ok());
        }
    }
}
