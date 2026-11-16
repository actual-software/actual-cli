//! The ADR files a repository already keeps where ADR tools put them, for
//! `actual adr discover`. Discovery only reads: it writes nothing and sends
//! nothing.
//!
//! It reads these folders, each with what names it:
//!
//! * Actual: `docs/adr/`, where `actual adr new` and Actual AI's sync write.
//! * adr-tools: the folder `.adr-dir` names, else `doc/adr/`.
//! * MADR: `docs/decisions/`.
//! * log4brains: each `adrFolder` in `.log4brains.yml`, packages included.
//! * Backstage: the `backstage.io/adr-location` annotation in the root
//!   `catalog-info.yaml`, a path from that file's folder.
//! * Common folders: `docs/adrs/`, `docs/architecture/decisions/`,
//!   `architecture/decisions/`, `adr/` and `decisions/`.
//!
//! A file's layout comes from its content: Actual's header; MADR's front
//! matter, sections or bulleted `Status:` line; or the Status, Context,
//! Decision and Consequences sections of Michael Nygard's template, which
//! adr-tools writes. A file in none of those layouts is listed as `other`
//! when a tool's folder holds it, and in a common folder only when its name
//! is numbered, such as `0007-use-postgres.md`. An index, a README or a
//! template is never an ADR.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path};

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::adr::files::{collect_markdown, split_header, DECISION_DIR, MAX_DEPTH, MAX_FILE_SIZE};
use crate::error::sanitize_server_text;

/// adr-tools' folder when `.adr-dir` names none.
const ADR_TOOLS_DIR: &str = "doc/adr";
/// MADR's folder.
const MADR_DIR: &str = "docs/decisions";
/// Folders many repositories use for ADRs without a tool naming them.
const COMMON_DIRS: &[&str] = &[
    "docs/adrs",
    "docs/architecture/decisions",
    "architecture/decisions",
    "adr",
    "decisions",
];
/// The Backstage annotation naming a component's ADR folder.
const ADR_LOCATION: &str = "backstage.io/adr-location";
/// Section headings only MADR uses, lowercased.
const MADR_SECTIONS: &[&str] = &[
    "context and problem statement",
    "considered options",
    "decision outcome",
];

/// What names a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FoundBy {
    Actual,
    AdrTools,
    Madr,
    Log4brains,
    Backstage,
    Common,
}

impl FoundBy {
    /// The name the text listing gives it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Actual => "Actual",
            Self::AdrTools => "adr-tools",
            Self::Madr => "MADR",
            Self::Log4brains => "log4brains",
            Self::Backstage => "Backstage",
            Self::Common => "common folder",
        }
    }
}

/// How an ADR file is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    /// Actual's header and Context File, which `actual adr create` sends.
    Actual,
    /// MADR, log4brains' template included.
    Madr,
    /// Michael Nygard's template, which adr-tools writes.
    Nygard,
    /// A numbered ADR file in none of those layouts.
    Other,
}

impl Layout {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Actual => "actual",
            Self::Madr => "madr",
            Self::Nygard => "nygard",
            Self::Other => "other",
        }
    }
}

/// A folder discovery read, or one a configuration file names that isn't
/// there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Folder {
    /// Relative to the repository root, `/`-separated.
    pub path: String,
    pub found_by: Vec<FoundBy>,
    pub exists: bool,
}

/// One ADR file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdrFile {
    /// Relative to the repository root.
    pub path: String,
    /// The folder it was found in, as [`Folder::path`] names it.
    pub folder: String,
    pub layout: Layout,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The `adr_id` an Actual header names. `actual adr new` writes a local
    /// id there, so an id alone doesn't mean Actual AI has the decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adr_id: Option<String>,
}

/// A file that couldn't be read, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unreadable {
    pub path: String,
    pub reason: String,
}

/// What discovery found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Discovery {
    /// Folders holding markdown, and folders a configuration file names.
    pub folders: Vec<Folder>,
    pub adrs: Vec<AdrFile>,
    /// Markdown in those folders that isn't an ADR, such as an index, a
    /// README or a template.
    pub other_files: Vec<String>,
    pub unreadable: Vec<Unreadable>,
    /// Configuration discovery couldn't use, and folders it wouldn't read.
    pub notes: Vec<String>,
}

/// The folders to read, in order, each with what names it and whether a
/// configuration file named it, so that one missing is still listed.
#[derive(Default)]
struct Wanted(Vec<(String, Vec<FoundBy>, bool)>);

impl Wanted {
    fn add(&mut self, path: &str, by: FoundBy, named: bool) {
        match self.0.iter_mut().find(|(known, _, _)| known == path) {
            Some((_, found_by, was_named)) => {
                if !found_by.contains(&by) {
                    found_by.push(by);
                }
                *was_named |= named;
            }
            None => self.0.push((path.to_string(), vec![by], named)),
        }
    }

    /// The folder `raw` names in configuration file `file`, or a note saying
    /// why it isn't read.
    fn add_named(&mut self, notes: &mut Vec<String>, file: &str, raw: &str, by: FoundBy) {
        match inside(raw) {
            Some(path) => self.add(&path, by, true),
            None => notes.push(format!(
                "{file} names \"{}\", which isn't a folder inside the repository, so it wasn't \
                 read.",
                sanitize_server_text(raw)
            )),
        }
    }
}

/// The ADR files under `root`, from the folders the module docs list.
pub fn discover(root: &Path) -> Discovery {
    let mut found = Discovery::default();
    let mut wanted = Wanted::default();
    wanted.add(DECISION_DIR, FoundBy::Actual, false);
    match config(root, ".adr-dir", &mut found.notes, |text| {
        Ok(text.lines().next().unwrap_or_default().trim().to_string())
    }) {
        Some(dir) => wanted.add_named(&mut found.notes, ".adr-dir", &dir, FoundBy::AdrTools),
        None => wanted.add(ADR_TOOLS_DIR, FoundBy::AdrTools, false),
    }
    wanted.add(MADR_DIR, FoundBy::Madr, false);
    if let Some(log4brains) = config(root, ".log4brains.yml", &mut found.notes, |text| {
        crate::yaml::from_repo_str::<Log4brains>(text).map_err(|e| e.to_string())
    }) {
        let project = log4brains.project;
        let packages = project
            .packages
            .iter()
            .filter_map(|p| p.adr_folder.as_ref());
        for dir in project.adr_folder.iter().chain(packages) {
            wanted.add_named(
                &mut found.notes,
                ".log4brains.yml",
                dir,
                FoundBy::Log4brains,
            );
        }
    }
    let catalog = config(root, "catalog-info.yaml", &mut found.notes, |text| {
        crate::yaml::from_repo_multiple::<CatalogEntity>(text).map_err(|e| e.to_string())
    });
    for location in catalog
        .iter()
        .flatten()
        .filter_map(|entity| entity.metadata.annotations.get(ADR_LOCATION))
    {
        if location.contains("://") || location.starts_with("url:") {
            found.notes.push(format!(
                "catalog-info.yaml names {}, a URL, which discover doesn't read.",
                sanitize_server_text(location)
            ));
        } else {
            wanted.add_named(
                &mut found.notes,
                "catalog-info.yaml",
                location,
                FoundBy::Backstage,
            );
        }
    }
    for dir in COMMON_DIRS {
        wanted.add(dir, FoundBy::Common, false);
    }

    let Ok(real_root) = root.canonicalize() else {
        return found;
    };
    let mut seen = HashSet::new();
    for (path, found_by, named) in wanted.0 {
        let dir = root.join(&path);
        let Some(real) = dir.canonicalize().ok().filter(|real| real.is_dir()) else {
            if named {
                found.folders.push(Folder {
                    path,
                    found_by,
                    exists: false,
                });
            }
            continue;
        };
        if !real.starts_with(&real_root) {
            found.notes.push(format!(
                "{path} leads outside the repository, so it wasn't read."
            ));
            continue;
        }
        let mut files = Vec::new();
        collect_markdown(&dir, MAX_DEPTH, &mut files);
        files.sort();
        // A folder inside another one, or a second name for it, lists each
        // file once.
        files.retain(|file| seen.insert(file.canonicalize().unwrap_or_else(|_| file.clone())));
        if files.is_empty() && !named {
            continue;
        }
        // Every markdown file in a tool's folder is an ADR; a common folder
        // may hold other notes.
        let tool_folder = found_by.iter().any(|by| *by != FoundBy::Common);
        for file in files {
            let shown = relative(root, &file);
            match read_adr(&file, tool_folder) {
                Ok(Some(adr)) => found.adrs.push(AdrFile {
                    path: shown,
                    folder: path.clone(),
                    ..adr
                }),
                Ok(None) => found.other_files.push(shown),
                Err(reason) => found.unreadable.push(Unreadable {
                    path: shown,
                    reason,
                }),
            }
        }
        found.folders.push(Folder {
            path,
            found_by,
            exists: true,
        });
    }
    found
}

/// `path` relative to `root`.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// `raw` as a folder inside the repository: relative, with `.` and `..`
/// resolved and `/` between its parts. `None` when it is absolute, names the
/// root itself or climbs out of it.
fn inside(raw: &str) -> Option<String> {
    let mut parts = Vec::new();
    for component in Path::new(raw.trim()).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Configuration file `file` at the root, read with `parse`. `None` when the
/// repository has no such file, or when it couldn't be read, which `notes`
/// then says.
fn config<T>(
    root: &Path,
    file: &str,
    notes: &mut Vec<String>,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Option<T> {
    let path = root.join(file);
    if !path.is_file() {
        return None;
    }
    read_text(&path)
        .and_then(|text| parse(&text))
        .map_err(|reason| notes.push(format!("{file} couldn't be read: {reason}")))
        .ok()
}

/// A file's text without a leading byte-order mark, refused over
/// [`MAX_FILE_SIZE`].
fn read_text(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE_SIZE {
        return Err(format!(
            "it is {size} bytes, over the {MAX_FILE_SIZE}-byte limit"
        ));
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    Ok(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => text,
    })
}

/// `.log4brains.yml`: `project.adrFolder`, and each package's.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Log4brains {
    project: Log4brainsProject,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Log4brainsProject {
    adr_folder: Option<String>,
    packages: Vec<Log4brainsPackage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Log4brainsPackage {
    adr_folder: Option<String>,
}

/// One entity of a Backstage `catalog-info.yaml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CatalogEntity {
    metadata: CatalogMetadata,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CatalogMetadata {
    annotations: BTreeMap<String, String>,
}

/// The front matter fields discovery reads: MADR's metadata, and the
/// `metadata.actual` block of Actual's header.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FrontMatter {
    title: Option<String>,
    status: Option<String>,
    date: Option<IgnoredAny>,
    deciders: Option<IgnoredAny>,
    #[serde(rename = "decision-makers")]
    decision_makers: Option<IgnoredAny>,
    consulted: Option<IgnoredAny>,
    informed: Option<IgnoredAny>,
    metadata: Option<HeaderMetadata>,
}

impl FrontMatter {
    /// The block between a file's `---` lines; empty when it isn't YAML
    /// discovery can read.
    fn of(header: &str) -> Self {
        let yaml: Vec<&str> = header
            .lines()
            .skip(1)
            .take_while(|line| line.trim_end() != "---")
            .collect();
        crate::yaml::from_repo_str(&yaml.join("\n")).unwrap_or_default()
    }

    fn is_madr(&self) -> bool {
        self.status.is_some()
            || self.date.is_some()
            || self.deciders.is_some()
            || self.decision_makers.is_some()
            || self.consulted.is_some()
            || self.informed.is_some()
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct HeaderMetadata {
    actual: Option<ActualFields>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ActualFields {
    adr_id: Option<String>,
    status: Option<String>,
}

/// What a file's body says about its title, sections and status.
#[derive(Debug, Default)]
struct Body {
    /// The first `# ` heading.
    title: Option<String>,
    /// Every `## ` heading, lowercased.
    sections: Vec<String>,
    /// The first `Status: …` line, and whether it was a list item.
    status_line: Option<(String, bool)>,
    /// The first line under a `## Status` heading.
    status_section: Option<String>,
}

impl Body {
    fn of(text: &str) -> Self {
        let mut body = Self::default();
        let mut fenced = false;
        let mut in_status = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with("```") || line.starts_with("~~~") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            if let Some(title) = line.strip_prefix("# ") {
                body.title = body.title.take().or_else(|| clean(title));
                in_status = false;
            } else if let Some(section) = line.strip_prefix("## ") {
                let section = clean(section).unwrap_or_default().to_lowercase();
                in_status = section == "status";
                body.sections.push(section);
            } else if line.starts_with('#') {
                in_status = false;
            } else {
                if in_status && body.status_section.is_none() {
                    body.status_section = clean(line);
                }
                body.status_line = body.status_line.take().or_else(|| status_line(line));
            }
        }
        body
    }

    fn has(&self, section: &str) -> bool {
        self.sections.iter().any(|s| s == section)
    }
}

/// Heading or value text without an HTML comment, a closing `#` run or
/// surrounding space; `None` when nothing is left.
fn clean(text: &str) -> Option<String> {
    let text = text.split("<!--").next().unwrap_or_default();
    let text = text.trim().trim_end_matches('#').trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The value of a `Status: …` line, and whether the line was a list item
/// (`- Status: …`, as MADR 2 and log4brains write it).
fn status_line(line: &str) -> Option<(String, bool)> {
    let item = line.strip_prefix("- ").or_else(|| line.strip_prefix("* "));
    let (key, value) = item.unwrap_or(line).split_once(':')?;
    let key = key.trim().trim_matches(['*', '_']);
    if !key.eq_ignore_ascii_case("status") {
        return None;
    }
    clean(value).map(|value| (value, item.is_some()))
}

/// A file name that marks an index, a README or a template, never an ADR.
fn not_an_adr(name: &str) -> bool {
    name == "readme" || name == "index" || name.ends_with("template")
}

/// A numbered file name: `0007-use-postgres`, `20200101_title`, `adr-12-x`.
fn numbered(name: &str) -> bool {
    let rest = name
        .strip_prefix("adr")
        .map_or(name, |rest| rest.trim_start_matches(['-', '_']));
    let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    digits > 0 && rest[digits..].starts_with(['-', '_', '.'])
}

/// One markdown file read as an ADR, without its path; `Ok(None)` when it
/// isn't one. In a tool's folder, `tool_folder`, any file but an index, a
/// README or a template is one.
fn read_adr(path: &Path, tool_folder: bool) -> Result<Option<AdrFile>, String> {
    let text = read_text(path)?;
    let (front, body) = match split_header(&text) {
        Some((header, body)) => (FrontMatter::of(header), Body::of(body)),
        None => (FrontMatter::default(), Body::of(&text)),
    };
    let title = body
        .title
        .clone()
        .or_else(|| front.title.as_deref().and_then(clean));
    let file = |layout, status, adr_id| {
        Ok(Some(AdrFile {
            path: String::new(),
            folder: String::new(),
            layout,
            title,
            status,
            adr_id,
        }))
    };
    if let Some(actual) = front.metadata.as_ref().and_then(|m| m.actual.as_ref()) {
        let id = actual
            .adr_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        return file(
            Layout::Actual,
            actual.status.as_deref().and_then(clean),
            id.map(str::to_string),
        );
    }
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if not_an_adr(&name) {
        return Ok(None);
    }
    let bulleted = body.status_line.as_ref().is_some_and(|(_, item)| *item);
    let layout = if front.is_madr() || bulleted || MADR_SECTIONS.iter().any(|s| body.has(s)) {
        Layout::Madr
    } else if body.has("context")
        && body.has("decision")
        && (body.has("status") || body.has("consequences"))
    {
        Layout::Nygard
    } else if tool_folder || numbered(&name) {
        Layout::Other
    } else {
        return Ok(None);
    };
    let status = front.status.as_deref().and_then(clean);
    let status = status
        .or(body.status_line.map(|(value, _)| value))
        .or(body.status_section);
    file(layout, status, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const ACTUAL: &str = "---\nversion: 2\nname: x\ndescription: Use Postgres\nmetadata:\n  \
        actual:\n    adr_id: 7d2cad87-0000-4000-8000-000000000001\n    status: accepted\n  \
        scope:\n    root: .\n---\n\n# Use Postgres for storage\n\nDate: 2026-11-11\n\n## Context\n\n\
        Why.\n\n## Decision\n\n1. MUST: Store relational data in Postgres.\n";

    const NYGARD: &str = "# 1. Record architecture decisions\n\nDate: 2026-01-02\n\n## Status\n\n\
        Accepted\n\n## Context\n\nWe need to record decisions.\n\n## Decision\n\nWe will use ADRs.\n\n\
        ## Consequences\n\nSee Nygard.\n";

    const MADR4: &str =
        "---\nstatus: accepted\ndate: 2026-01-03\ndecision-makers: Platform\n---\n\n\
        # Use MADR\n\n## Context and Problem Statement\n\nHow to record?\n";

    const LOG4BRAINS: &str = "# Use log4brains\n\n- Status: draft <!-- optional -->\n- Date: \
        2026-01-04\n\n## Context and Problem Statement\n\nWhy.\n";

    fn adr<'a>(found: &'a Discovery, path: &str) -> &'a AdrFile {
        found
            .adrs
            .iter()
            .find(|adr| adr.path == path)
            .unwrap_or_else(|| panic!("{path} not in {found:#?}"))
    }

    #[test]
    fn reads_each_tool_folder_and_each_layout() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "docs/adr/7d2cad87-use-postgres.md", ACTUAL);
        write(
            root,
            "docs/adr/drafts/0002-later.md",
            "# Later\n\nStatus: proposed\n",
        );
        write(
            root,
            "docs/adr/README.md",
            "# ADRs\n\n## Context\n\n## Decision\n\n## Status\n",
        );
        write(
            root,
            "docs/adr/generated.md",
            "Prose from an older generator.\n\n## Policies\n",
        );
        write(
            root,
            "doc/adr/0001-record-architecture-decisions.md",
            NYGARD,
        );
        write(root, "docs/decisions/0003-use-madr.md", MADR4);
        write(root, "docs/decisions/adr-template.md", MADR4);
        write(root, "docs/adrs/20260104-use-log4brains.md", LOG4BRAINS);
        write(root, "decisions/notes.md", "# Meeting notes\n");
        write(root, "decisions/0004-plain-text.md", "Just text.\n");
        write(root, "adr/main.go", "package adr\n");

        let found = discover(root);

        let actual = adr(&found, "docs/adr/7d2cad87-use-postgres.md");
        assert_eq!(actual.folder, "docs/adr");
        assert_eq!(actual.layout, Layout::Actual);
        assert_eq!(actual.title.as_deref(), Some("Use Postgres for storage"));
        assert_eq!(actual.status.as_deref(), Some("accepted"));
        assert_eq!(
            actual.adr_id.as_deref(),
            Some("7d2cad87-0000-4000-8000-000000000001")
        );
        let later = adr(&found, "docs/adr/drafts/0002-later.md");
        assert_eq!(later.layout, Layout::Other);
        assert_eq!(later.status.as_deref(), Some("proposed"));
        let nygard = adr(&found, "doc/adr/0001-record-architecture-decisions.md");
        assert_eq!(nygard.layout, Layout::Nygard);
        assert_eq!(
            nygard.title.as_deref(),
            Some("1. Record architecture decisions")
        );
        assert_eq!(nygard.status.as_deref(), Some("Accepted"));
        assert_eq!(nygard.adr_id, None);
        let madr = adr(&found, "docs/decisions/0003-use-madr.md");
        assert_eq!(madr.layout, Layout::Madr);
        assert_eq!(madr.status.as_deref(), Some("accepted"));
        let log4brains = adr(&found, "docs/adrs/20260104-use-log4brains.md");
        assert_eq!(log4brains.layout, Layout::Madr);
        assert_eq!(log4brains.status.as_deref(), Some("draft"));
        let other = adr(&found, "decisions/0004-plain-text.md");
        assert_eq!(
            (other.layout, other.title.as_deref()),
            (Layout::Other, None)
        );
        // A tool's folder holds only ADRs, whatever their layout.
        let generated = adr(&found, "docs/adr/generated.md");
        assert_eq!(
            (generated.layout, generated.status.as_deref()),
            (Layout::Other, None)
        );
        assert_eq!(found.adrs.len(), 7);

        assert_eq!(
            found.other_files,
            [
                "docs/adr/README.md",
                "docs/decisions/adr-template.md",
                "decisions/notes.md"
            ]
        );
        let folders: Vec<(&str, Vec<FoundBy>, bool)> = found
            .folders
            .iter()
            .map(|f| (f.path.as_str(), f.found_by.clone(), f.exists))
            .collect();
        assert_eq!(
            folders,
            [
                ("docs/adr", vec![FoundBy::Actual], true),
                ("doc/adr", vec![FoundBy::AdrTools], true),
                ("docs/decisions", vec![FoundBy::Madr], true),
                ("docs/adrs", vec![FoundBy::Common], true),
                ("decisions", vec![FoundBy::Common], true),
            ]
        );
        assert!(found.unreadable.is_empty() && found.notes.is_empty());
    }

    #[test]
    fn configuration_files_name_folders() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, ".adr-dir", "architecture/records\nignored\n");
        write(root, "architecture/records/0001-x.md", NYGARD);
        write(
            root,
            ".log4brains.yml",
            "project:\n  name: x\n  adrFolder: ./docs/adr\n  packages:\n    - name: api\n      \
             adrFolder: ./packages/api/docs/adr\n    - name: web\n      path: ./web\n",
        );
        write(root, "packages/api/docs/adr/20260101-api.md", LOG4BRAINS);
        write(
            root,
            "catalog-info.yaml",
            "apiVersion: backstage.io/v1alpha1\nkind: Component\nmetadata:\n  name: a\n  \
             annotations:\n    backstage.io/adr-location: docs/records\n---\nkind: Group\n\
             metadata:\n  name: b\n  annotations:\n    backstage.io/adr-location: \
             https://example.com/adrs\n---\nmetadata:\n  annotations:\n    \
             backstage.io/adr-location: ../elsewhere\n",
        );

        let found = discover(root);

        let folders: Vec<(&str, Vec<FoundBy>, bool)> = found
            .folders
            .iter()
            .map(|f| (f.path.as_str(), f.found_by.clone(), f.exists))
            .collect();
        assert_eq!(
            folders,
            [
                (
                    "docs/adr",
                    vec![FoundBy::Actual, FoundBy::Log4brains],
                    false
                ),
                ("architecture/records", vec![FoundBy::AdrTools], true),
                ("packages/api/docs/adr", vec![FoundBy::Log4brains], true),
                ("docs/records", vec![FoundBy::Backstage], false),
            ]
        );
        assert_eq!(found.adrs.len(), 2);
        assert_eq!(
            found.notes,
            [
                "catalog-info.yaml names https://example.com/adrs, a URL, which discover doesn't \
                 read.",
                "catalog-info.yaml names \"../elsewhere\", which isn't a folder inside the \
                 repository, so it wasn't read."
            ]
        );
    }

    #[test]
    fn unreadable_configuration_and_files_are_noted() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".log4brains.yml"), [0xff, 0xfe, 0x00]).unwrap();
        write(root, "catalog-info.yaml", "metadata: [\n");
        write(root, ".adr-dir", "/etc\n");
        write(
            root,
            "docs/adr/0001-big.md",
            &"x".repeat(MAX_FILE_SIZE as usize + 1),
        );
        std::fs::write(root.join("docs/adr/0002-binary.md"), [0xff, 0xfe]).unwrap();

        let found = discover(root);

        assert_eq!(found.unreadable.len(), 2, "{found:#?}");
        assert!(found.unreadable[0]
            .reason
            .contains("over the 1048576-byte limit"));
        assert_eq!(found.unreadable[1].path, "docs/adr/0002-binary.md");
        assert_eq!(found.notes.len(), 3, "{:#?}", found.notes);
        assert!(found.notes[0].starts_with(".adr-dir names \"/etc\""));
        assert!(found.notes[1].starts_with(".log4brains.yml couldn't be read: "));
        assert!(found.notes[2].starts_with("catalog-info.yaml couldn't be read: "));
        assert!(found.adrs.is_empty());
    }

    #[test]
    fn a_missing_root_finds_nothing() {
        let tmp = tempdir().unwrap();
        let found = discover(&tmp.path().join("missing"));
        assert_eq!(found, Discovery::default());
    }

    #[cfg(unix)]
    #[test]
    fn folders_and_files_are_read_once_and_never_outside_the_root() {
        let outside = tempdir().unwrap();
        write(outside.path(), "0001-x.md", NYGARD);
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(root, "docs/adr/0001-x.md", NYGARD);
        std::fs::create_dir_all(root.join("docs/architecture")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("docs/architecture/decisions"))
            .unwrap();
        std::os::unix::fs::symlink(root.join("docs/adr"), root.join("adr")).unwrap();
        write(
            root,
            ".log4brains.yml",
            "project:\n  adrFolder: docs/adr/\n  packages:\n    - adrFolder: docs/adr\n",
        );

        let found = discover(root);

        assert_eq!(found.adrs.len(), 1, "{found:#?}");
        assert_eq!(
            found.notes,
            ["docs/architecture/decisions leads outside the repository, so it wasn't read."]
        );
        assert_eq!(found.folders.len(), 1);
        assert_eq!(
            found.folders[0].found_by,
            [FoundBy::Actual, FoundBy::Log4brains]
        );
    }

    #[test]
    fn inside_keeps_relative_folders_in_the_repository() {
        assert_eq!(inside(" ./docs//adr/ ").as_deref(), Some("docs/adr"));
        assert_eq!(inside("a/../b").as_deref(), Some("b"));
        assert_eq!(inside("."), None);
        assert_eq!(inside("../x"), None);
        assert_eq!(inside("/abs"), None);
    }

    #[test]
    fn body_reads_titles_sections_and_status_lines() {
        let body = Body::of(
            "```\n# Not a title\n## Status\n```\n# Title ##\n# Second\n## Status <!-- x -->\n\n\
             **Status**: superseded\n### Detail\nignored\n## Decision\n",
        );
        assert_eq!(body.title.as_deref(), Some("Title"));
        assert_eq!(body.sections, ["status", "decision"]);
        assert_eq!(
            body.status_section.as_deref(),
            Some("**Status**: superseded")
        );
        assert_eq!(body.status_line, Some(("superseded".to_string(), false)));
        assert_eq!(
            status_line("* status: Accepted"),
            Some(("Accepted".to_string(), true))
        );
        assert_eq!(status_line("Date: 2026-01-01"), None);
        assert_eq!(status_line("no colon"), None);
        assert_eq!(status_line("Status:  "), None);
        assert_eq!(clean("  <!-- only a comment -->"), None);
    }

    #[test]
    fn file_names_mark_numbers_and_non_adrs() {
        for name in [
            "0007-use-postgres",
            "20200101_title",
            "adr-12-x",
            "adr_3.x",
            "1.x",
        ] {
            assert!(numbered(name), "{name}");
        }
        for name in ["use-postgres", "adr", "007", "12abc", "adr-x"] {
            assert!(!numbered(name), "{name}");
        }
        for name in ["readme", "index", "template", "adr-template"] {
            assert!(not_an_adr(name), "{name}");
        }
        assert!(!not_an_adr("0001-templates-for-services"));
    }

    /// `read_adr` with a common folder's rule.
    fn read_adr_common(path: &Path) -> Result<Option<AdrFile>, String> {
        read_adr(path, false)
    }

    #[test]
    fn read_adr_takes_the_title_from_front_matter_and_reads_odd_headers() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "a.md",
            "\u{feff}---\ntitle: From front matter\ndeciders: [a]\n---\n\nNo heading.\n",
        );
        let madr = read_adr_common(&root.join("a.md")).unwrap().unwrap();
        assert_eq!(madr.layout, Layout::Madr);
        assert_eq!(madr.title.as_deref(), Some("From front matter"));
        assert_eq!(madr.status, None);
        write(
            root,
            "b.md",
            "---\nmetadata:\n  actual:\n    adr_id: \"  \"\n---\n# Draft\n",
        );
        let actual = read_adr_common(&root.join("b.md")).unwrap().unwrap();
        assert_eq!(
            (actual.layout, actual.adr_id, actual.status),
            (Layout::Actual, None, None)
        );
        write(
            root,
            "c.md",
            "---\nstatus: [not, text]\n---\n# Plain notes\n",
        );
        assert_eq!(read_adr_common(&root.join("c.md")).unwrap(), None);
        for (name, front) in [
            ("d", "date: 2026-01-01"),
            ("e", "consulted: x"),
            ("f", "informed: y"),
            ("g", "decision-makers: z"),
        ] {
            write(
                root,
                &format!("{name}.md"),
                &format!("---\n{front}\n---\n# T\n"),
            );
            let read = read_adr_common(&root.join(format!("{name}.md")))
                .unwrap()
                .unwrap();
            assert_eq!(read.layout, Layout::Madr, "{front}");
        }
        write(
            root,
            "h.md",
            "# T\n\n## Context\n\n## Decision\n\n## Consequences\n",
        );
        let nygard = read_adr_common(&root.join("h.md")).unwrap().unwrap();
        assert_eq!((nygard.layout, nygard.status), (Layout::Nygard, None));
        write(root, "i.md", "# T\n\n## Context\n\n## Decision\n");
        assert_eq!(read_adr_common(&root.join("i.md")).unwrap(), None);
        write(
            root,
            "j.md",
            "# T\n\n## Status\n\n## Context\n\n## Considered Options\n",
        );
        let madr = read_adr_common(&root.join("j.md")).unwrap().unwrap();
        assert_eq!(madr.layout, Layout::Madr);
    }

    #[test]
    fn labels_name_each_folder_source_and_layout() {
        let labels: Vec<&str> = [
            FoundBy::Actual,
            FoundBy::AdrTools,
            FoundBy::Madr,
            FoundBy::Log4brains,
            FoundBy::Backstage,
            FoundBy::Common,
        ]
        .into_iter()
        .map(FoundBy::label)
        .collect();
        assert_eq!(
            labels,
            [
                "Actual",
                "adr-tools",
                "MADR",
                "log4brains",
                "Backstage",
                "common folder"
            ]
        );
        let layouts: Vec<&str> = [Layout::Actual, Layout::Madr, Layout::Nygard, Layout::Other]
            .into_iter()
            .map(Layout::as_str)
            .collect();
        assert_eq!(layouts, ["actual", "madr", "nygard", "other"]);
        assert_eq!(
            serde_json::to_value(FoundBy::AdrTools).unwrap(),
            serde_json::json!("adr-tools")
        );
    }
}
