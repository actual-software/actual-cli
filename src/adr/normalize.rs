//! A decision doc in another format turned into a Context File, for
//! `actual adr normalize`.
//!
//! # Design
//!
//! Conversion runs in tiers, so most files need neither a model nor a person:
//!
//! 1. [`read`] maps what a known layout already says. A Context File, a
//!    Nygard ADR (Status / Context / Decision / Consequences), a MADR
//!    (front matter, Context and Problem Statement, Decision Outcome …) and
//!    adr-bot's layout (`## Policies`) all map without a model: a decision
//!    line with an upper-case RFC-2119 keyword is a policy, and so is every
//!    bullet under a `## Policies` heading (at MUST, as Actual AI reads a
//!    policy with no level).
//! 2. When that finds no policy — a decision written as prose — a model
//!    fills the gaps. Its answer, or one an agent wrote (`--extracted`), is an
//!    [`Extraction`] laid over what [`read`] found, and so are a person's
//!    answers (`--answers`).
//! 3. [`plan`] turns the result into a decision, or into the questions that
//!    are still open: is this a decision at all, which policies, governing or
//!    draft, which paths. With no person to ask (`--yes`), each open question
//!    takes the safe answer: the decision is written as a draft, which no
//!    check reads, and a file with no policy is not written.
//!
//! Nothing the author wrote is lost. Sections the Context File has no place
//! for are carried over after it, verbatim, under their own headings; a
//! heading the Context File parser reads (such as `Decision`) is renamed
//! `Original Decision` so it doesn't shadow the converted one.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::adr::context_file::{
    h1_title, h2_heading, js_trim, split_lines, Consequences, ContextFile, Policy,
};
use crate::adr::files::{header_yaml, split_header};
use crate::adr::pair::rule_spelling;
use crate::adr::save_check::check_document;
use crate::rules::types::RuleLevel;

/// The fields a conversion settles. Every one is optional: [`read`] fills
/// what the file says, a model or an agent fills the rest, and a person's
/// answers override both. The same shape is what `--extracted` and
/// `--answers` take for each file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extraction {
    /// Whether the file records a decision at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_adr: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// As the file says it (`Accepted`, `Proposed` …) or as the header
    /// takes it (`accepted`, `draft` …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deciders: Option<String>,
    /// `YYYY-MM-DD`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Vec<String>>,
    /// Each as `LEVEL: statement`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policies: Option<Vec<String>>,
    /// Globs from the repository root; `!` excludes; empty for the whole
    /// repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<Vec<String>>,
    /// Fields the extractor isn't sure of: `is_adr`, `status`, `policies`
    /// or `scope`. Each is asked about.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncertain: Vec<String>,
    /// For a possible duplicate: write it anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_duplicate: Option<bool>,
}

impl Extraction {
    /// `self` with every field `top` sets replaced by it. A field `top` sets
    /// is no longer uncertain, unless `top` says so itself.
    pub fn overlay(&self, top: &Extraction) -> Extraction {
        fn pick<T: Clone>(top: &Option<T>, base: &Option<T>) -> Option<T> {
            top.clone().or_else(|| base.clone())
        }
        let set = [
            ("is_adr", top.is_adr.is_some()),
            ("status", top.status.is_some()),
            ("policies", top.policies.is_some()),
            ("scope", top.scope.is_some()),
        ];
        let mut uncertain: Vec<String> = self
            .uncertain
            .iter()
            .filter(|field| !set.iter().any(|(name, is_set)| *is_set && name == field))
            .cloned()
            .collect();
        for field in &top.uncertain {
            if !uncertain.contains(field) {
                uncertain.push(field.clone());
            }
        }
        Extraction {
            is_adr: pick(&top.is_adr, &self.is_adr),
            title: pick(&top.title, &self.title),
            status: pick(&top.status, &self.status),
            deciders: pick(&top.deciders, &self.deciders),
            date: pick(&top.date, &self.date),
            context: pick(&top.context, &self.context),
            policies: pick(&top.policies, &self.policies),
            scope: pick(&top.scope, &self.scope),
            rationale: pick(&top.rationale, &self.rationale),
            uncertain,
            keep_duplicate: pick(&top.keep_duplicate, &self.keep_duplicate),
        }
    }

    fn is_uncertain(&self, field: &str) -> bool {
        self.uncertain.iter().any(|f| f == field)
    }

    /// Whether the policies need a model: none were found.
    pub fn needs_model(&self) -> bool {
        self.policies.as_ref().is_none_or(Vec::is_empty)
    }
}

/// What [`read`] found in a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Read {
    /// The fields the file's layout gives.
    pub found: Extraction,
    pub consequences: Option<Consequences>,
    pub references: Vec<String>,
    /// Sections carried over after the Context File, as `(heading, body)`.
    pub carried: Vec<(String, String)>,
}

/// A heading's text, lowered, without a leading `3.` and a trailing `:`.
fn heading_key(heading: &str) -> String {
    static NUMBER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\d+[.)]?\s+").expect("valid regex"));
    let lowered = js_trim(heading).to_lowercase();
    let unnumbered = NUMBER.replace(&lowered, "");
    unnumbered.trim_end_matches(':').trim().to_string()
}

/// Headings the Context File parser reads; a carried section with one of
/// these is renamed so it doesn't shadow the converted section.
const PARSER_KEYS: &[&str] = &[
    "context",
    "decision",
    "policy block",
    "rationale",
    "consequences",
    "references",
];

const CONTEXT_KEYS: &[&str] = &[
    "context",
    "context and problem statement",
    "problem statement",
    "problem",
    "background",
];
const DECISION_KEYS: &[&str] = &[
    "decision",
    "decisions",
    "decision outcome",
    "policy block",
    "policies",
    "policy",
    "rules",
];
/// Decision headings whose every bullet is a policy, leveled or not.
const POLICY_LIST_KEYS: &[&str] = &["policies", "policy", "rules", "policy block"];
const RATIONALE_KEYS: &[&str] = &["rationale", "decision drivers", "justification"];
const CONSEQUENCE_KEYS: &[&str] = &[
    "consequences",
    "instructions",
    "positive consequences",
    "negative consequences",
];
const REFERENCE_KEYS: &[&str] = &["references", "links", "see also", "related"];

/// An upper-case RFC-2119 keyword, the longer spellings first.
static RFC_KEYWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(MUST NOT|MUST|SHALL NOT|SHALL|SHOULD NOT|SHOULD|NOT RECOMMENDED|RECOMMENDED|REQUIRED|MAY|OPTIONAL)\b",
    )
    .expect("valid regex")
});

/// A list marker or a rule id at the start of a line.
static MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[-*+]\s+|\d+[.)]\s+)?(?:\*\*R-[A-Za-z0-9-]+\*\*\s*)?").expect("valid regex")
});

/// An ADR number before a title: `ADR-0003:`, `7.`, `0007 -`.
static TITLE_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:adr[-\s]?)?\d+\s*[.:)\-]?\s+").expect("valid regex"));

/// `line` without its list marker, and whether it had one.
fn unmarked(line: &str) -> (&str, bool) {
    let line = js_trim(line);
    let marker = MARKER.find(line).map_or(0, |found| found.end());
    (js_trim(&line[marker..]), marker > 0)
}

/// A decision line as a policy: one with an upper-case RFC-2119 keyword, or
/// any bullet under a heading that lists policies (at MUST). The keyword is
/// dropped when it leads the line (`MUST: x`), and the whole sentence kept
/// when it doesn't (`Services MUST use x`).
fn policy_line(line: &str) -> Option<String> {
    let (text, _) = unmarked(line);
    let keyword = RFC_KEYWORD.find(text)?.as_str();
    let level = RuleLevel::split_prefix(keyword).map(|(level, _)| level)?;
    let statement = RuleLevel::split_prefix(text)
        .filter(|_| text.starts_with(keyword))
        .map_or(text, |(_, rest)| rest);
    Some(format!("{}: {statement}", rule_spelling(level)))
}

/// A section's items: each non-empty line without its list marker, less
/// sub-headings.
fn items(body: &str) -> Vec<String> {
    split_lines(body)
        .map(|line| unmarked(line).0)
        .filter(|text| !text.is_empty() && !text.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// A bullet under a policy-list heading with no keyword, at MUST.
fn unleveled_policy(line: &str) -> Option<String> {
    let (text, bulleted) = unmarked(line);
    (bulleted && !text.is_empty()).then(|| format!("MUST: {text}"))
}

/// The status a header takes for what a file says: `accepted`, `draft`,
/// `superseded`, `deprecated` or `rejected`; `None` when it says nothing this
/// recognises.
pub fn header_status(said: &str) -> Option<&'static str> {
    let lowered = js_trim(said).to_lowercase();
    let (word, _) = unmarked(&lowered);
    let word = word
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .find(|w| !w.is_empty())?;
    Some(match word {
        "accepted"
        | "approved"
        | "adopted"
        | "decided"
        | "active"
        | "implemented"
        | "created-from-user"
        | "created-from-suggestion"
        | "updated" => "accepted",
        "proposed" | "draft" | "pending" | "review" | "open" | "wip" => "draft",
        "superseded" => "superseded",
        "deprecated" | "retired" | "obsolete" => "deprecated",
        "rejected" | "declined" | "abandoned" => "rejected",
        _ => return None,
    })
}

/// Front matter fields a MADR or a hand-written ADR may carry, and the
/// status a broken decision header still names.
fn front_matter(header: &str) -> Extraction {
    let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(&header_yaml(header)) else {
        return Extraction::default();
    };
    let text = |v: &serde_yaml::Value| match v {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Sequence(items) => Some(
            items
                .iter()
                .filter_map(serde_yaml::Value::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        _ => None,
    };
    Extraction {
        title: text(&value["title"]),
        status: text(&value["status"]).or_else(|| text(&value["metadata"]["actual"]["status"])),
        date: text(&value["date"]),
        deciders: text(&value["deciders"]),
        ..Extraction::default()
    }
}

/// The preamble's `Label:` field, with an optional list marker before it.
fn preamble_label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let (text, _) = unmarked(line);
    let head = text.get(..label.len())?;
    head.eq_ignore_ascii_case(label)
        .then(|| js_trim(text[label.len()..].trim_start_matches(':')))
}

/// Read what a decision doc's layout says. `text` is the whole file.
pub fn read(text: &str) -> Read {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let (mut found, body) = match split_header(text) {
        Some((header, body)) => (front_matter(header), body),
        None => (Extraction::default(), text),
    };
    let mut out = Read::default();

    let mut preamble: Vec<&str> = Vec::new();
    let mut sections: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in split_lines(body) {
        match h2_heading(line).map(js_trim).filter(|h| !h.is_empty()) {
            Some(heading) => sections.push((heading, Vec::new())),
            None => match sections.last_mut() {
                Some((_, lines)) => lines.push(line),
                None => preamble.push(line),
            },
        }
    }

    // The preamble: the title, its fields, and any prose, which is context.
    let preamble_text = preamble.join("\n");
    if let Some(title) = h1_title(&preamble_text) {
        found.title = Some(TITLE_NUMBER.replace(title, "").trim().to_string());
    }
    let mut context: Vec<String> = Vec::new();
    for line in &preamble {
        let trimmed = js_trim(line);
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("---") {
            continue;
        }
        if let Some(value) = preamble_label(line, "Status") {
            found.status.get_or_insert_with(|| value.to_string());
        } else if let Some(value) = preamble_label(line, "Date") {
            found.date.get_or_insert_with(|| value.to_string());
        } else if let Some(value) = preamble_label(line, "Deciders") {
            found.deciders.get_or_insert_with(|| value.to_string());
        } else {
            context.extend(items(trimmed));
        }
    }

    let mut policies: Vec<String> = Vec::new();
    let mut rationale: Vec<String> = Vec::new();
    for (heading, lines) in sections {
        let key = heading_key(heading);
        let body = js_trim(&lines.join("\n")).to_string();
        let carry_as = |out: &mut Read, kept: String| {
            let renamed = if PARSER_KEYS.contains(&key.as_str()) {
                format!("Original {heading}")
            } else {
                heading.to_string()
            };
            out.carried.push((renamed, kept));
        };
        let k = key.as_str();
        if k == "status" {
            if let Some(line) = lines.iter().map(|l| js_trim(l)).find(|l| !l.is_empty()) {
                found.status = Some(line.to_string());
            }
        } else if CONTEXT_KEYS.contains(&k) {
            context.extend(items(&body));
        } else if DECISION_KEYS.contains(&k) {
            let policy_list = POLICY_LIST_KEYS.contains(&k);
            let mut leftover: Vec<&str> = Vec::new();
            for line in &lines {
                let policy = policy_line(line)
                    .or_else(|| policy_list.then(|| unleveled_policy(line)).flatten());
                match policy {
                    Some(policy) => policies.push(policy),
                    None if !js_trim(line).is_empty() => leftover.push(line),
                    None => {}
                }
            }
            if !leftover.is_empty() {
                carry_as(&mut out, body);
            }
        } else if RATIONALE_KEYS.contains(&k) {
            rationale.extend(items(&body));
        } else if CONSEQUENCE_KEYS.contains(&k) {
            match consequences(&body) {
                Some(mapped) => {
                    let into = out.consequences.get_or_insert_with(Consequences::default);
                    into.positive.extend(mapped.positive);
                    into.negative.extend(mapped.negative);
                }
                None if !body.is_empty() => carry_as(&mut out, body),
                None => {}
            }
        } else if REFERENCE_KEYS.contains(&k) {
            out.references.extend(items(&body));
        } else {
            carry_as(&mut out, body);
        }
    }
    if !context.is_empty() {
        found.context = Some(context);
    }
    if !policies.is_empty() {
        found.policies = Some(policies);
    }
    if !rationale.is_empty() {
        found.rationale = Some(rationale);
    }
    out.found = found;
    out
}

/// A Consequences section as the Context File's two lists: labeled blocks
/// (`Positive:` then bullets), labeled bullets (`- Positive: x`, adr-bot's
/// layout) and MADR's `Good, because x` / `Bad, because x`. `None` when
/// any line fits none of these, so the section is carried as written.
fn consequences(body: &str) -> Option<Consequences> {
    static LABELED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)^(positive|negative|good|bad|pro|con)\b[,:]?\s*(?:because\s+)?(.*)$")
            .expect("valid regex")
    });
    let mut found = Consequences::default();
    let mut block: Option<bool> = None;
    for line in split_lines(body) {
        let (text, _) = unmarked(line);
        if text.is_empty() {
            continue;
        }
        let (positive, item) = match LABELED.captures(text) {
            Some(caps) => {
                let positive = matches!(
                    caps[1].to_ascii_lowercase().as_str(),
                    "positive" | "good" | "pro"
                );
                let rest = js_trim(caps.get(2).map_or("", |m| m.as_str()));
                if rest.is_empty() {
                    block = Some(positive);
                    continue;
                }
                (positive, rest)
            }
            None => (block?, text),
        };
        let list = if positive {
            &mut found.positive
        } else {
            &mut found.negative
        };
        list.push(item.to_string());
    }
    (!found.positive.is_empty() || !found.negative.is_empty()).then_some(found)
}

/// One question [`plan`] needs answered. The answer goes in the field of
/// the file's [`Extraction`] that `field` names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Question {
    /// `keep_duplicate`, `is_adr`, `policies`, `status` or `scope`.
    pub field: &'static str,
    pub question: String,
    /// The answers to choose from, when there is a fixed set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// What the conversion would use: the extracted policies or globs, or
    /// the safe answer.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggested: Vec<String>,
}

/// A decision ready to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    /// The checked decision.
    pub doc: ContextFile,
    /// The header status.
    pub status: String,
    pub scope: Vec<String>,
    /// Safe answers taken for open questions, for the report.
    pub defaulted: Vec<String>,
}

/// What [`plan`] decided for a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Ready(Box<Ready>),
    NeedsInput(Vec<Question>),
    /// Not written, with the reason.
    Skip(String),
}

/// How [`plan`] settles a file.
pub struct Settings<'a> {
    /// The file looks like a decision the repository has already.
    pub duplicate: bool,
    /// `--allow-duplicate`.
    pub allow_duplicate: bool,
    /// `--yes`: take the safe answer to every open question.
    pub yes: bool,
    /// Today, `YYYY-MM-DD`, for a decision whose file has no date.
    pub today: &'a str,
    /// The title when the file has none, from its name.
    pub fallback_title: &'a str,
    /// Whether a glob matches a file in the repository.
    pub matches: &'a dyn Fn(&str) -> bool,
}

/// A date the Context File keeps: `YYYY-MM-DD`.
fn is_date(text: &str) -> bool {
    static DATE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("valid regex"));
    DATE.is_match(text)
}

/// Settle a file: the decision to write, the questions still open, or why
/// it isn't written.
pub fn plan(read: &Read, merged: &Extraction, settings: &Settings) -> Plan {
    let mut questions: Vec<Question> = Vec::new();
    let mut defaulted: Vec<String> = Vec::new();
    let mut draft = false;

    if settings.duplicate && !settings.allow_duplicate {
        match merged.keep_duplicate {
            Some(true) => {}
            Some(false) => return Plan::Skip("Kept out as a duplicate.".to_string()),
            None if settings.yes => {
                return Plan::Skip(
                    "It looks like a decision this repository has already; answer \
                     keep_duplicate or pass --allow-duplicate to write it anyway."
                        .to_string(),
                )
            }
            None => questions.push(Question {
                field: "keep_duplicate",
                question: "It looks like a decision this repository already has. Write it anyway?"
                    .to_string(),
                options: vec!["true".to_string(), "false".to_string()],
                suggested: vec!["false".to_string()],
            }),
        }
    }

    if merged.is_adr == Some(false) {
        return Plan::Skip("It doesn't record a decision.".to_string());
    }
    if merged.is_uncertain("is_adr") {
        if settings.yes {
            return Plan::Skip(
                "It may not record a decision; answer is_adr to convert it.".to_string(),
            );
        }
        questions.push(Question {
            field: "is_adr",
            question: "Does this file record an architecture decision?".to_string(),
            options: vec!["true".to_string(), "false".to_string()],
            suggested: Vec::new(),
        });
    }

    let mut policies: Vec<Policy> = Vec::new();
    let mut unleveled: Vec<String> = Vec::new();
    for text in merged.policies.iter().flatten() {
        match RuleLevel::split_prefix(text) {
            Some((level, statement)) if !statement.is_empty() => policies.push(Policy {
                level,
                statement: statement.to_string(),
            }),
            _ => unleveled.push(text.clone()),
        }
    }
    let ask_policies = |question: String, suggested: Vec<String>| Question {
        field: "policies",
        question,
        options: Vec::new(),
        suggested,
    };
    if policies.is_empty() || !unleveled.is_empty() {
        if settings.yes {
            return Plan::Skip(if policies.is_empty() {
                "No policy found: answer policies (each `LEVEL: statement`) to convert it."
                    .to_string()
            } else {
                format!("A policy has no level: {}", unleveled.join("; "))
            });
        }
        questions.push(ask_policies(
            "What does this decision require? Give each policy as `LEVEL: statement`, LEVEL \
             being MUST, MUST NOT, SHOULD, SHOULD NOT or MAY."
                .to_string(),
            merged.policies.clone().unwrap_or_default(),
        ));
    } else if merged.is_uncertain("policies") {
        if settings.yes {
            draft = true;
            defaulted.push("The policies were extracted by a model, so it is a draft.".to_string());
        } else {
            questions.push(ask_policies(
                "Are these the decision's policies? Answer with the list to use.".to_string(),
                merged.policies.clone().unwrap_or_default(),
            ));
        }
    }

    let mut status = merged.status.as_deref().and_then(header_status);
    if status.is_none() || merged.is_uncertain("status") {
        if settings.yes {
            status = Some(status.unwrap_or("draft"));
            if merged.is_uncertain("status") || merged.status.is_none() {
                draft = true;
                defaulted
                    .push("The file doesn't say whether it governs, so it is a draft.".to_string());
            } else {
                defaulted.push(format!(
                    "Its status \"{}\" isn't one Actual knows, so it is a draft.",
                    merged.status.clone().unwrap_or_default()
                ));
            }
        } else {
            questions.push(Question {
                field: "status",
                question: "Should this decision govern now (accepted), or wait as a draft?"
                    .to_string(),
                options: vec!["accepted".to_string(), "draft".to_string()],
                suggested: vec!["draft".to_string()],
            });
        }
    }

    let mut scope: Vec<String> = merged
        .scope
        .iter()
        .flatten()
        .map(|glob| js_trim(glob).to_string())
        .filter(|glob| !glob.is_empty())
        .collect();
    let unmatched: Vec<String> = scope
        .iter()
        .filter(|glob| !glob.starts_with('!') && !(settings.matches)(glob))
        .cloned()
        .collect();
    if !unmatched.is_empty() || merged.is_uncertain("scope") {
        if settings.yes {
            scope.retain(|glob| !unmatched.contains(glob));
            draft = true;
            defaulted.push(if unmatched.is_empty() {
                "Its scope was a guess, so it is a draft.".to_string()
            } else {
                format!(
                    "No file matches {}, so it was dropped and the decision is a draft.",
                    unmatched.join(", ")
                )
            });
        } else {
            questions.push(Question {
                field: "scope",
                question: if unmatched.is_empty() {
                    "Which paths does this decision apply to? An empty list is the whole repository."
                        .to_string()
                } else {
                    format!(
                        "No file matches {}. Which paths does this decision apply to? An empty \
                         list is the whole repository.",
                        unmatched.join(", ")
                    )
                },
                options: Vec::new(),
                suggested: scope.iter().filter(|g| !unmatched.contains(g)).cloned().collect(),
            });
        }
    }

    if !questions.is_empty() {
        return Plan::NeedsInput(questions);
    }
    let mut status = status.unwrap_or("draft").to_string();
    if draft && status == "accepted" {
        status = "draft".to_string();
    }

    let title = merged
        .title
        .as_deref()
        .map(js_trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(settings.fallback_title)
        .to_string();
    let doc = ContextFile {
        title,
        deciders: merged.deciders.clone().unwrap_or_default(),
        context: merged.context.clone().unwrap_or_default(),
        policies,
        rationale: merged.rationale.clone().unwrap_or_default(),
        consequences: read.consequences.clone(),
        references: read.references.clone(),
        ..ContextFile::default()
    };
    let mut doc = match check_document(&doc, status != "accepted", settings.today) {
        Ok(doc) => doc,
        Err(reasons) => return Plan::Skip(reasons.join(" ")),
    };
    if let Some(date) = merged.date.as_deref().map(js_trim).filter(|d| is_date(d)) {
        doc.date = date.to_string();
    }
    Plan::Ready(Box::new(Ready {
        doc,
        status,
        scope,
        defaulted,
    }))
}

/// A title from a file name: `0007-use-postgres.md` gives `Use postgres`.
pub fn title_from_name(stem: &str) -> String {
    static LEADING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}|(?i:adr)?[-_]?\d+)[-_ .]*")
            .expect("valid regex")
    });
    let rest = LEADING.replace(stem, "");
    let words = rest.replace(['-', '_'], " ");
    let words = js_trim(&words);
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Untitled".to_string(),
    }
}

/// The JSON schema a model answers with: an [`Extraction`].
pub const MODEL_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "is_adr": {"type": "boolean"},
    "title": {"type": "string"},
    "status": {"type": "string", "enum": ["accepted", "draft", "superseded", "deprecated", "rejected", "unknown"]},
    "context": {"type": "array", "items": {"type": "string"}},
    "policies": {"type": "array", "items": {"type": "string"}},
    "scope": {"type": "array", "items": {"type": "string"}},
    "uncertain": {"type": "array", "items": {"type": "string", "enum": ["is_adr", "status", "policies", "scope"]}}
  },
  "required": ["is_adr", "title", "status", "context", "policies", "scope", "uncertain"]
}"#;

/// The longest file a model is shown, in bytes.
const MODEL_INPUT_MAX: usize = 60_000;

/// The prompt that asks a model to extract a decision from `text`, the file
/// at `path`. `dirs` are the repository's top-level directories, for scope.
pub fn model_prompt(path: &str, text: &str, dirs: &[String]) -> String {
    let mut cut = text.len().min(MODEL_INPUT_MAX);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "You convert an architecture decision record (ADR) into structured fields. Answer only \
         from what the file says; never invent a requirement.\n\n\
         Fields:\n\
         - is_adr: false when the file records no decision (an index, notes, a template).\n\
         - title: the decision's title on one line, without an ADR number.\n\
         - status: accepted, draft (proposed or undecided), superseded, deprecated or rejected; \
           unknown when the file doesn't say.\n\
         - context: why the decision was needed, one short point per item.\n\
         - policies: what the decision requires, each as `LEVEL: statement` on one line. LEVEL is \
           MUST, MUST NOT, SHOULD, SHOULD NOT or MAY. Write each statement as a checkable rule \
           about the code (\"Services store relational data in Postgres\"), not as history \
           (\"We chose Postgres\").\n\
         - scope: globs from the repository root for the code the decision governs, only when \
           the file names specific paths or packages; an empty list means the whole repository. \
           Top-level directories: {dirs}.\n\
         - uncertain: every field you had to guess.\n\n\
         File {path}:\n<adr>\n{body}\n</adr>\n",
        dirs = if dirs.is_empty() {
            "(none)".to_string()
        } else {
            dirs.join(", ")
        },
        body = &text[..cut],
    )
}

/// A model's answer as an [`Extraction`]. A status of `unknown` is left out,
/// so it is asked about.
pub fn from_model(value: serde_json::Value) -> Result<Extraction, String> {
    let mut extraction: Extraction = serde_json::from_value(value)
        .map_err(|e| format!("The model's answer didn't fit the schema: {e}"))?;
    if extraction.status.as_deref() == Some("unknown") {
        extraction.status = None;
    }
    Ok(extraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: &str = "2026-11-19";

    fn settings<'a>(matches: &'a dyn Fn(&str) -> bool) -> Settings<'a> {
        Settings {
            duplicate: false,
            allow_duplicate: false,
            yes: false,
            today: TODAY,
            fallback_title: "From name",
            matches,
        }
    }

    fn any(_: &str) -> bool {
        true
    }

    fn none(_: &str) -> bool {
        false
    }

    fn ready(p: Plan) -> Ready {
        assert!(matches!(p, Plan::Ready(_)), "{p:#?}");
        let Plan::Ready(r) = p else { panic!() };
        *r
    }

    fn asked(p: Plan) -> Vec<Question> {
        assert!(matches!(p, Plan::NeedsInput(_)), "{p:#?}");
        let Plan::NeedsInput(q) = p else { panic!() };
        q
    }

    fn questions(plan: Plan) -> Vec<&'static str> {
        asked(plan).iter().map(|q| q.field).collect()
    }

    fn skip(p: Plan) -> String {
        assert!(matches!(p, Plan::Skip(_)), "{p:#?}");
        let Plan::Skip(r) = p else { panic!() };
        r
    }

    const NYGARD: &str = "# 7. Use Postgres for storage

Date: 2024-03-02

## Status

Accepted

## Context

Each service picked its own database.
Backups differ per service.

## Decision

We will use one relational database.

- Services MUST store relational data in Postgres.
- MUST NOT: open SQLite files outside tests.

## Consequences

Operations get simpler, but migrations need care.
";

    #[test]
    fn reads_a_nygard_adr_and_carries_what_it_cannot_map() {
        let read = read(NYGARD);
        let found = &read.found;
        assert_eq!(found.title.as_deref(), Some("Use Postgres for storage"));
        assert_eq!(found.status.as_deref(), Some("Accepted"));
        assert_eq!(found.date.as_deref(), Some("2024-03-02"));
        assert_eq!(
            found.context.as_deref(),
            Some(
                &[
                    "Each service picked its own database.".to_string(),
                    "Backups differ per service.".to_string()
                ][..]
            )
        );
        assert_eq!(
            found.policies.as_deref(),
            Some(
                &[
                    "MUST: Services MUST store relational data in Postgres.".to_string(),
                    "MUST NOT: open SQLite files outside tests.".to_string()
                ][..]
            )
        );
        assert!(!found.needs_model());
        let headings: Vec<&str> = read.carried.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(headings, ["Original Decision", "Original Consequences"]);
        assert!(read.carried[0]
            .1
            .contains("We will use one relational database."));

        let plan = ready(plan(&read, &read.found, &settings(&any)));
        assert_eq!(plan.status, "accepted");
        assert_eq!(plan.doc.date, "2024-03-02");
        assert_eq!(plan.doc.policies.len(), 2);
        assert_eq!(plan.doc.policies[1].level, RuleLevel::MustNot);
        assert!(plan.defaulted.is_empty());
    }

    const MADR: &str = "---
status: proposed
date: 2025-01-10
deciders:
  - Ana
  - Bo
---
# Use structured logging

## Context and Problem Statement

Logs are hard to search.

## Decision Drivers

* Searchability

## Considered Options

* JSON lines
* Plain text

## Decision Outcome

Chosen option: JSON lines.

* Services MUST log JSON lines.

### Consequences

* Good, because logs are searchable.
* Bad, because they are harder to read raw.

## Pros and Cons of the Options

* Good, because one format
* Bad, because tooling

## More Information

See the wiki.
";

    #[test]
    fn reads_a_madr_with_front_matter() {
        let read = read(MADR);
        let found = &read.found;
        assert_eq!(found.title.as_deref(), Some("Use structured logging"));
        assert_eq!(found.status.as_deref(), Some("proposed"));
        assert_eq!(found.date.as_deref(), Some("2025-01-10"));
        assert_eq!(found.deciders.as_deref(), Some("Ana, Bo"));
        assert_eq!(
            found.context.as_deref(),
            Some(&["Logs are hard to search.".to_string()][..])
        );
        assert_eq!(
            found.rationale.as_deref(),
            Some(&["Searchability".to_string()][..])
        );
        assert_eq!(
            found.policies.as_deref(),
            Some(&["MUST: Services MUST log JSON lines.".to_string()][..])
        );
        // Pros and cons of the options aren't the decision's consequences.
        assert_eq!(read.consequences, None);
        let headings: Vec<&str> = read.carried.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(
            headings,
            [
                "Considered Options",
                "Decision Outcome",
                "Pros and Cons of the Options",
                "More Information"
            ]
        );
        assert!(read.carried[1]
            .1
            .contains("Good, because logs are searchable."));

        let plan = ready(plan(&read, &read.found, &settings(&any)));
        assert_eq!(plan.status, "draft");
        assert_eq!(plan.doc.deciders, "Ana, Bo");
    }

    const LEGACY: &str = "The codebase requires consistent testing.
It should be modular.
## Policies
- Implement a modular library architecture.
- Tests MUST use the shared fixtures.
## Instructions
- Positive: Clear separation of concerns
- Negative: More modules to organise
";

    #[test]
    fn reads_adr_bot_layout() {
        let read = read(LEGACY);
        let found = &read.found;
        assert_eq!(found.title, None);
        assert_eq!(found.status, None);
        assert_eq!(found.context.as_ref().unwrap().len(), 2);
        assert_eq!(
            found.policies.as_deref(),
            Some(
                &[
                    "MUST: Implement a modular library architecture.".to_string(),
                    "MUST: Tests MUST use the shared fixtures.".to_string()
                ][..]
            )
        );
        let consequences = read.consequences.clone().unwrap();
        assert_eq!(consequences.positive, ["Clear separation of concerns"]);
        assert_eq!(consequences.negative, ["More modules to organise"]);
        assert!(read.carried.is_empty());

        // No status: asked, or a draft with --yes; the title comes from the name.
        assert_eq!(
            questions(plan(&read, &read.found, &settings(&any))),
            ["status"]
        );
        let mut yes = settings(&any);
        yes.yes = true;
        let plan = ready(plan(&read, &read.found, &yes));
        assert_eq!(plan.status, "draft");
        assert_eq!(plan.doc.title, "From name");
        assert_eq!(plan.defaulted.len(), 1);
    }

    #[test]
    fn reads_a_context_file_and_block_consequences() {
        let text = "# Use API keys\n\nStatus: accepted\nDeciders: Platform\n\n## Context\n\n- Users need auth.\n\n\
                    ## Decision\n\n1. MUST: Require an API key.\n\n## Rationale\n\n- Simple.\n\n\
                    ## Consequences\n\nPositive:\n- Easy.\n\nNegative:\n- Rotation.\n\n## References\n\n- RFC 6750\n";
        let read = read(text);
        assert_eq!(read.found.deciders.as_deref(), Some("Platform"));
        assert_eq!(
            read.found.policies.as_deref(),
            Some(&["MUST: Require an API key.".to_string()][..])
        );
        assert_eq!(
            read.found.rationale.as_deref(),
            Some(&["Simple.".to_string()][..])
        );
        assert_eq!(read.consequences.clone().unwrap().negative, ["Rotation."]);
        assert_eq!(read.references, ["RFC 6750"]);
        assert!(read.carried.is_empty());
        let plan = ready(plan(&read, &read.found, &settings(&any)));
        assert_eq!(plan.doc.references, ["RFC 6750"]);
        assert_eq!(plan.doc.date, TODAY);
    }

    #[test]
    fn prose_decisions_need_a_model_and_empty_sections_are_dropped() {
        let read = read("\u{feff}# ADR-0003: Cache sessions\n\n* Status: Proposed\n\n## Decision\n\nWe cache sessions in Redis.\n\n## Consequences\n\n## Notes\n");
        assert_eq!(read.found.title.as_deref(), Some("Cache sessions"));
        assert_eq!(read.found.status.as_deref(), Some("Proposed"));
        assert!(read.found.needs_model());
        let headings: Vec<&str> = read.carried.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(headings, ["Original Decision", "Notes"]);
        // Without policies a person is asked; with --yes it isn't written.
        assert_eq!(
            questions(plan(&read, &read.found, &settings(&any))),
            ["policies"]
        );
        let mut yes = settings(&any);
        yes.yes = true;
        assert!(skip(plan(&read, &read.found, &yes)).contains("No policy found"));
    }

    #[test]
    fn unreadable_front_matter_and_heading_variants() {
        let read = read("---\n: [\n---\n# T\n\n## 3. Decision:\n\n- SHOULD prefer small modules.\n\n## Links\n\n- x\n\n## Status\n\n");
        assert_eq!(read.found.status, None);
        assert_eq!(
            read.found.policies.as_deref(),
            Some(&["SHOULD: prefer small modules.".to_string()][..])
        );
        assert_eq!(read.references, ["x"]);
        let read = super::read("---\nmetadata:\n  actual:\n    status: accepted\n---\n# T\n");
        assert_eq!(read.found.status.as_deref(), Some("accepted"));
        let read = super::read("---\ntitle: 7\n---\n");
        assert_eq!(read.found.title, None);
    }

    #[test]
    fn header_status_maps_what_files_say() {
        for (said, status) in [
            ("Accepted", Some("accepted")),
            ("- approved on 2024-01-01", Some("accepted")),
            ("Proposed", Some("draft")),
            ("In review", None),
            ("review", Some("draft")),
            ("Superseded by ADR-0009", Some("superseded")),
            ("Deprecated", Some("deprecated")),
            ("Rejected", Some("rejected")),
            ("created-from-user", Some("accepted")),
            ("mystery", None),
            ("", None),
        ] {
            assert_eq!(header_status(said), status, "{said}");
        }
    }

    #[test]
    fn overlay_replaces_fields_and_settles_uncertainty() {
        let base = Extraction {
            title: Some("A".to_string()),
            status: Some("draft".to_string()),
            uncertain: vec!["status".to_string(), "scope".to_string()],
            ..Extraction::default()
        };
        let top = Extraction {
            status: Some("accepted".to_string()),
            uncertain: vec!["policies".to_string(), "scope".to_string()],
            ..Extraction::default()
        };
        let merged = base.overlay(&top);
        assert_eq!(merged.title.as_deref(), Some("A"));
        assert_eq!(merged.status.as_deref(), Some("accepted"));
        assert_eq!(merged.uncertain, ["scope", "policies"]);
    }

    fn found(policies: &[&str]) -> Extraction {
        Extraction {
            title: Some("Use Postgres".to_string()),
            status: Some("accepted".to_string()),
            policies: Some(policies.iter().map(|p| p.to_string()).collect()),
            ..Extraction::default()
        }
    }

    #[test]
    fn duplicates_are_asked_about_unless_allowed() {
        let read = Read::default();
        let merged = found(&["MUST: Use Postgres."]);
        let mut s = settings(&any);
        s.duplicate = true;
        assert_eq!(questions(plan(&read, &merged, &s)), ["keep_duplicate"]);
        let keep = merged.overlay(&Extraction {
            keep_duplicate: Some(true),
            ..Extraction::default()
        });
        ready(plan(&read, &keep, &s));
        let drop = merged.overlay(&Extraction {
            keep_duplicate: Some(false),
            ..Extraction::default()
        });
        assert!(skip(plan(&read, &drop, &s)).contains("duplicate"));
        s.yes = true;
        assert!(skip(plan(&read, &merged, &s)).contains("allow-duplicate"));
        s.allow_duplicate = true;
        ready(plan(&read, &merged, &s));
    }

    #[test]
    fn not_an_adr_is_skipped_and_doubt_is_asked() {
        let read = Read::default();
        let mut merged = found(&["MUST: Use Postgres."]);
        merged.is_adr = Some(false);
        assert!(skip(plan(&read, &merged, &settings(&any))).contains("doesn't record"));
        merged.is_adr = None;
        merged.uncertain = vec!["is_adr".to_string()];
        assert_eq!(questions(plan(&read, &merged, &settings(&any))), ["is_adr"]);
        let mut yes = settings(&any);
        yes.yes = true;
        assert!(skip(plan(&read, &merged, &yes)).contains("is_adr"));
    }

    #[test]
    fn model_policies_are_confirmed_or_drafted() {
        let read = Read::default();
        let mut merged = found(&["MUST: Use Postgres."]);
        merged.uncertain = vec!["policies".to_string()];
        assert_eq!(
            questions(plan(&read, &merged, &settings(&any))),
            ["policies"]
        );
        let mut yes = settings(&any);
        yes.yes = true;
        let ready = ready(plan(&read, &merged, &yes));
        assert_eq!(ready.status, "draft");
        assert_eq!(ready.defaulted.len(), 1);
    }

    #[test]
    fn unleveled_policies_are_asked_about() {
        let read = Read::default();
        let merged = found(&["MUST: Use Postgres.", "Prefer small modules."]);
        assert_eq!(
            questions(plan(&read, &merged, &settings(&any))),
            ["policies"]
        );
        let mut yes = settings(&any);
        yes.yes = true;
        assert!(skip(plan(&read, &merged, &yes)).contains("Prefer small modules."));
    }

    #[test]
    fn status_doubt_and_unknown_statuses() {
        let read = Read::default();
        let mut merged = found(&["MUST: Use Postgres."]);
        merged.uncertain = vec!["status".to_string()];
        assert_eq!(questions(plan(&read, &merged, &settings(&any))), ["status"]);
        let mut yes = settings(&any);
        yes.yes = true;
        assert_eq!(ready(plan(&read, &merged, &yes)).status, "draft");
        merged.uncertain.clear();
        merged.status = Some("mystery".to_string());
        let r = ready(plan(&read, &merged, &yes));
        assert_eq!(r.status, "draft");
        assert!(r.defaulted[0].contains("mystery"));
    }

    #[test]
    fn scope_globs_must_match_a_file() {
        let read = Read::default();
        let mut merged = found(&["MUST: Use Postgres."]);
        merged.scope = Some(vec![
            "services/**".to_string(),
            "!services/legacy/**".to_string(),
            " ".to_string(),
        ]);
        assert_eq!(
            ready(plan(&read, &merged, &settings(&any))).scope,
            ["services/**", "!services/legacy/**"]
        );
        let q = asked(plan(&read, &merged, &settings(&none)));
        assert_eq!(q[0].field, "scope");
        assert!(q[0].question.contains("services/**"));
        assert_eq!(q[0].suggested, ["!services/legacy/**"]);
        let mut yes = settings(&none);
        yes.yes = true;
        let r = ready(plan(&read, &merged, &yes));
        assert_eq!(r.scope, ["!services/legacy/**"]);
        assert_eq!(r.status, "draft");
        // A guessed scope is asked about too.
        merged.scope = Some(Vec::new());
        merged.uncertain = vec!["scope".to_string()];
        let q = asked(plan(&read, &merged, &settings(&any)));
        assert!(q[0].question.starts_with("Which paths"));
        assert!(ready(plan(&read, &merged, &yes)).defaulted[0].contains("guess"));
    }

    #[test]
    fn a_decision_the_save_check_refuses_is_skipped() {
        let read = Read::default();
        let mut merged = found(&["MUST: Use Postgres."]);
        merged.title = Some("Two\nlines".to_string());
        assert!(skip(plan(&read, &merged, &settings(&any))).contains("one line"));
    }

    #[test]
    fn titles_from_names() {
        assert_eq!(title_from_name("0007-use-postgres"), "Use postgres");
        assert_eq!(title_from_name("adr-12_cache_sessions"), "Cache sessions");
        assert_eq!(
            title_from_name("0b6f2a1e-1111-4222-8333-444455556666-adopt-modules"),
            "Adopt modules"
        );
        assert_eq!(title_from_name("0007"), "Untitled");
    }

    #[test]
    fn model_prompt_and_answer() {
        let prompt = model_prompt(
            "docs/adr/a.md",
            &format!("a{}", "é".repeat(MODEL_INPUT_MAX)),
            &["src".to_string()],
        );
        assert!(prompt.contains("Top-level directories: src."));
        assert!(prompt.contains("File docs/adr/a.md:"));
        assert!(model_prompt("a", "x", &[]).contains("(none)"));
        let schema: serde_json::Value = serde_json::from_str(MODEL_SCHEMA).unwrap();
        assert_eq!(schema["type"], "object");

        let answer = from_model(serde_json::json!({
            "is_adr": true, "title": "T", "status": "unknown", "context": [],
            "policies": ["MUST: x"], "scope": [], "uncertain": ["status"]
        }))
        .unwrap();
        assert_eq!(answer.status, None);
        assert_eq!(
            answer.policies.as_deref(),
            Some(&["MUST: x".to_string()][..])
        );
        assert!(from_model(serde_json::json!({"surprise": 1})).is_err());
    }
}
