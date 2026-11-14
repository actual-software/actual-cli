//! The version 2 YAML header Actual AI's repository sync writes at the top of
//! both files it keeps for a decision (`docs/adr/` and `.actual/rules/`).
//!
//! The sync builds the header with js-yaml's `dump` (block style, two-space
//! indent, no line width, double quotes). This module writes the same bytes for
//! the fields the CLI sets, including js-yaml's choice between a plain and a
//! double-quoted scalar, so the next sync does not rewrite a header the CLI
//! wrote. The one known difference: js-yaml writes a value holding a line break
//! as a literal block (`|-`); this module double-quotes it, which reads back as
//! the same string.

use std::sync::LazyLock;

use regex::Regex;

/// The header format version.
const HEADER_VERSION: u32 = 2;
/// Longest description the sync writes, in JavaScript (UTF-16) units.
const DESCRIPTION_MAX_UNITS: usize = 1024;
/// The scope root that stands for the whole repository.
pub(crate) const REPOSITORY_ROOT: &str = ".";

/// The fields of the header this CLI writes. `scope_paths` may hold `!`-prefixed excludes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdrHeader {
    /// Stem of the file the header heads (its name without `.md`).
    pub name: String,
    /// One-line description; see [`description_for`].
    pub description: String,
    /// The decision id.
    pub adr_id: String,
    /// The decision's lifecycle status, such as `created-from-user` or `accepted`.
    pub status: String,
    /// The repository-relative directory the paths are written against; `.` is
    /// the whole repository.
    pub scope_root: String,
    /// Globs relative to `scope_root`; a `!` prefix makes an exclude.
    pub scope_paths: Vec<String>,
}

/// The header block, from the opening `---` line through the closing `---` line and its newline, laid out and quoted the way the sync's YAML dump writes it (field order version, name, description, metadata.actual.adr_id, status, metadata.scope.root, paths when non-empty).
///
/// `scope_root` is normalised as the sync does before it writes it: leading
/// `./` and trailing `/` dropped, and an empty root written as `.`.
pub fn render_header(header: &AdrHeader) -> String {
    let mut out = format!("---\nversion: {HEADER_VERSION}\n");
    push_field(&mut out, "", "name", &header.name);
    push_field(&mut out, "", "description", &header.description);
    out.push_str("metadata:\n  actual:\n");
    push_field(&mut out, "    ", "adr_id", &header.adr_id);
    push_field(&mut out, "    ", "status", &header.status);
    out.push_str("  scope:\n");
    push_field(
        &mut out,
        "    ",
        "root",
        &normalize_scope_root(&header.scope_root),
    );
    if !header.scope_paths.is_empty() {
        out.push_str("    paths:\n");
        for path in &header.scope_paths {
            out.push_str("      - ");
            out.push_str(&scalar(path));
            out.push('\n');
        }
    }
    out.push_str("---\n");
    out
}

/// A header description for a title: whitespace runs collapsed to one space, trimmed, and cut to 1,024 characters, as the sync does.
///
/// Whitespace and length are counted as JavaScript counts them: the sync's
/// `\s` (which takes in U+FEFF but not U+0085) and UTF-16 units. An astral
/// character that would straddle the cut is dropped whole, where the sync would
/// keep half of it.
pub fn description_for(title: &str) -> String {
    let mut collapsed = String::with_capacity(title.len());
    let mut after_space = false;
    for c in title.chars() {
        if is_js_space(c) {
            after_space = true;
            continue;
        }
        if after_space && !collapsed.is_empty() {
            collapsed.push(' ');
        }
        after_space = false;
        collapsed.push(c);
    }
    let mut units = 0;
    if let Some((cut, _)) = collapsed.char_indices().find(|&(_, c)| {
        units += c.len_utf16();
        units > DESCRIPTION_MAX_UNITS
    }) {
        collapsed.truncate(cut);
        collapsed.truncate(collapsed.trim_end_matches(' ').len());
    }
    collapsed
}

/// The scope root as the sync writes it: trimmed, leading `./` and trailing `/`
/// dropped, and `.` when nothing is left.
pub(crate) fn normalize_scope_root(root: &str) -> String {
    let mut root = root.trim_matches(is_js_space);
    while let Some(rest) = root.strip_prefix("./") {
        root = rest;
    }
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        REPOSITORY_ROOT.to_string()
    } else {
        root.to_string()
    }
}

/// Append `<indent><key>: <value>` and a newline.
fn push_field(out: &mut String, indent: &str, key: &str, value: &str) {
    out.push_str(indent);
    out.push_str(key);
    out.push_str(": ");
    out.push_str(&scalar(value));
    out.push('\n');
}

/// JavaScript's `\s`: its WhiteSpace and LineTerminator characters.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

// ── js-yaml's scalar style, for a block mapping value or sequence item ──────

/// Booleans of YAML 1.1 that js-yaml quotes in its default compatibility mode.
const DEPRECATED_BOOLEANS: [&str; 16] = [
    "y", "Y", "yes", "Yes", "YES", "on", "On", "ON", "n", "N", "no", "No", "NO", "off", "Off",
    "OFF",
];

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

/// YAML 1.1 sexagesimal numbers, also quoted for compatibility.
static DEPRECATED_BASE60: LazyLock<Regex> =
    LazyLock::new(|| compile(r"^[-+]?[0-9_]+(?::[0-9_]+)+(?:\.[0-9_]*)?$"));
/// js-yaml's float pattern (finite values are checked separately).
static YAML_FLOAT: LazyLock<Regex> = LazyLock::new(|| {
    compile(concat!(
        r"^(?:[-+]?(?:[0-9]+)(?:\.[0-9]*)?(?:[eE][-+]?[0-9]+)?",
        r"|\.[0-9]+(?:[eE][-+]?[0-9]+)?",
        r"|[-+]?\.(?:inf|Inf|INF)",
        r"|\.(?:nan|NaN|NAN))$",
    ))
});
/// js-yaml's date pattern.
static YAML_DATE: LazyLock<Regex> =
    LazyLock::new(|| compile(r"^([0-9][0-9][0-9][0-9])-([0-9][0-9])-([0-9][0-9])$"));
/// js-yaml's timestamp pattern.
static YAML_TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    compile(concat!(
        r"^([0-9][0-9][0-9][0-9])-([0-9][0-9]?)-([0-9][0-9]?)",
        r"(?:[Tt]|[ \t]+)([0-9][0-9]?):([0-9][0-9]):([0-9][0-9])",
        r"(?:\.([0-9]*))?(?:[ \t]*(Z|([-+])([0-9][0-9]?)(?::([0-9][0-9]))?))?$",
    ))
});

/// `value` as js-yaml writes it with `quotingType: '"'`: plain when it can be,
/// otherwise double-quoted.
fn scalar(value: &str) -> String {
    if value.is_empty()
        || DEPRECATED_BOOLEANS.contains(&value)
        || DEPRECATED_BASE60.is_match(value)
        || !is_plain(value)
        || is_ambiguous(value)
    {
        double_quoted(value)
    } else {
        value.to_string()
    }
}

/// js-yaml's `isPrintable`: what it writes without an escape.
fn is_printable(c: char) -> bool {
    matches!(c, '\u{20}'..='\u{7E}' | '\u{10000}'..='\u{10FFFF}')
        || (matches!(c, '\u{A1}'..='\u{D7FF}') && c != '\u{2028}' && c != '\u{2029}')
        || (matches!(c, '\u{E000}'..='\u{FFFD}') && c != '\u{FEFF}')
}

/// YAML's `s-white`.
fn is_white(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// js-yaml's `isNsCharOrWhitespace`.
fn is_ns_char_or_white(c: char) -> bool {
    is_printable(c) && c != '\u{FEFF}' && c != '\r' && c != '\n'
}

/// js-yaml's `isPlainSafe` in block context, where flow indicators are allowed:
/// a `#` only after a non-space, a `:` only before a non-space.
fn is_plain_safe(c: char, prev: Option<char>) -> bool {
    let c_ns_or_white = is_ns_char_or_white(c);
    let c_ns = c_ns_or_white && !is_white(c);
    let prev_colon = prev == Some(':');
    (c_ns_or_white && c != '#' && !(prev_colon && !c_ns))
        || (prev.is_some_and(|p| is_ns_char_or_white(p) && !is_white(p)) && c == '#')
        || (prev_colon && c_ns)
}

/// js-yaml's `isPlainSafeFirst`: not white space and not a YAML indicator.
fn is_plain_safe_first(c: char) -> bool {
    is_printable(c)
        && c != '\u{FEFF}'
        && !is_white(c)
        && !matches!(
            c,
            '-' | '?'
                | ':'
                | ','
                | '['
                | ']'
                | '{'
                | '}'
                | '#'
                | '&'
                | '*'
                | '!'
                | '|'
                | '='
                | '>'
                | '\''
                | '"'
                | '%'
                | '@'
                | '`'
        )
}

/// True when js-yaml's style check allows a plain scalar: a safe first and last
/// character and every character printable and plain-safe. A line feed is not
/// printable here, which turns js-yaml's literal block into a quoted string.
fn is_plain(value: &str) -> bool {
    let safe_first = value.chars().next().is_some_and(is_plain_safe_first);
    let safe_last = value
        .chars()
        .next_back()
        .is_some_and(|last| !is_white(last) && last != ':');
    if !safe_first || !safe_last {
        return false;
    }
    let mut prev = None;
    for c in value.chars() {
        if !is_printable(c) || !is_plain_safe(c, prev) {
            return false;
        }
        prev = Some(c);
    }
    true
}

/// True when a plain `value` would read back as another type in js-yaml's
/// default schema: null, bool, int, float, timestamp or merge key.
fn is_ambiguous(value: &str) -> bool {
    matches!(value, "~" | "null" | "Null" | "NULL")
        || matches!(
            value,
            "true" | "True" | "TRUE" | "false" | "False" | "FALSE"
        )
        || is_yaml_int(value)
        || is_yaml_float(value)
        || YAML_DATE.is_match(value)
        || YAML_TIMESTAMP.is_match(value)
        || value == "<<"
}

/// js-yaml's integer resolver: decimal, or `0b`, `0x` or `0o` digits, with an
/// optional sign.
///
/// js-yaml also requires the value to be finite as a double. That is checked
/// for decimal digits; a binary, hex or octal run too long for a double (over
/// 1,000 bits) is still read as an integer here, so it is quoted, which reads
/// back as the same string.
fn is_yaml_int(value: &str) -> bool {
    let unsigned = value.strip_prefix(['-', '+']).unwrap_or(value).as_bytes();
    if unsigned == b"0" {
        return true;
    }
    let radix_digits = match unsigned {
        [b'0', b'b', digits @ ..] => Some((digits, b"01".as_slice())),
        [b'0', b'x', digits @ ..] => Some((digits, b"0123456789abcdefABCDEF".as_slice())),
        [b'0', b'o', digits @ ..] => Some((digits, b"01234567".as_slice())),
        _ => None,
    };
    match radix_digits {
        Some((digits, allowed)) => !digits.is_empty() && digits.iter().all(|d| allowed.contains(d)),
        None => {
            !unsigned.is_empty()
                && unsigned.iter().all(u8::is_ascii_digit)
                && value.parse::<f64>().is_ok_and(f64::is_finite)
        }
    }
}

/// js-yaml's float resolver: its pattern, and a finite value unless the text
/// is `.inf` or `.nan` (which no Rust parse reads, so a failed parse counts).
fn is_yaml_float(value: &str) -> bool {
    YAML_FLOAT.is_match(value) && value.parse::<f64>().map_or(true, f64::is_finite)
}

/// `value` in double quotes, escaped as js-yaml escapes it.
fn double_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        let escape = match c {
            '\0' => "\\0",
            '\u{07}' => "\\a",
            '\u{08}' => "\\b",
            '\t' => "\\t",
            '\n' => "\\n",
            '\u{0B}' => "\\v",
            '\u{0C}' => "\\f",
            '\r' => "\\r",
            '\u{1B}' => "\\e",
            '"' => "\\\"",
            '\\' => "\\\\",
            '\u{85}' => "\\N",
            '\u{A0}' => "\\_",
            '\u{2028}' => "\\L",
            '\u{2029}' => "\\P",
            c if is_printable(c) => {
                out.push(c);
                continue;
            }
            // Every astral character is printable, so only these two widths occur.
            c if u32::from(c) <= 0xFF => {
                out.push_str(&format!("\\x{:02X}", u32::from(c)));
                continue;
            }
            c => {
                out.push_str(&format!("\\u{:04X}", u32::from(c)));
                continue;
            }
        };
        out.push_str(escape);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(description: &str, root: &str, paths: &[&str]) -> AdrHeader {
        AdrHeader {
            name: "cross-cutting-use-postgres-storage-0b6f".to_string(),
            description: description.to_string(),
            adr_id: "0b6f2a1e-1111-2222-3333-444455556666".to_string(),
            status: "created-from-user".to_string(),
            scope_root: root.to_string(),
            scope_paths: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    /// Parse `rendered` back with a YAML parser and check every field survives.
    fn assert_round_trips(rendered: &str, expected: &AdrHeader) {
        let body = rendered
            .strip_prefix("---\n")
            .and_then(|rest| rest.strip_suffix("---\n"))
            .expect("fenced header");
        let value: serde_yaml::Value = serde_yaml::from_str(body).expect("valid YAML");
        let text = |v: &serde_yaml::Value| v.as_str().map(str::to_string);
        assert_eq!(value["version"].as_u64(), Some(2));
        assert_eq!(text(&value["name"]), Some(expected.name.clone()));
        assert_eq!(
            text(&value["description"]),
            Some(expected.description.clone())
        );
        let actual = &value["metadata"]["actual"];
        assert_eq!(text(&actual["adr_id"]), Some(expected.adr_id.clone()));
        assert_eq!(text(&actual["status"]), Some(expected.status.clone()));
        let scope = &value["metadata"]["scope"];
        assert_eq!(
            text(&scope["root"]),
            Some(normalize_scope_root(&expected.scope_root))
        );
        let paths: Vec<String> = scope["paths"]
            .as_sequence()
            .map(|items| items.iter().filter_map(text).collect())
            .unwrap_or_default();
        assert_eq!(paths, expected.scope_paths);
    }

    #[test]
    fn renders_the_cli_create_header() {
        let h = header("Use Postgres for storage", ".", &[]);
        let rendered = render_header(&h);
        assert_eq!(
            rendered,
            "---\nversion: 2\nname: cross-cutting-use-postgres-storage-0b6f\n\
             description: Use Postgres for storage\nmetadata:\n  actual:\n    \
             adr_id: 0b6f2a1e-1111-2222-3333-444455556666\n    status: created-from-user\n  \
             scope:\n    root: .\n---\n"
        );
        assert_round_trips(&rendered, &h);
    }

    #[test]
    fn renders_the_real_example_quoting() {
        let h = AdrHeader {
            name: "cross-cutting-activities-returning-validation-errors-p-fdbb".to_string(),
            description: "@temporalio/common Library Adoption: Activities Returning Validation \
                Errors Pass Structured"
                .to_string(),
            adr_id: "fdbbcb01-e293-46dd-a7af-ca1f67f66c5b".to_string(),
            status: "accepted".to_string(),
            scope_root: "backend".to_string(),
            scope_paths: vec![
                "src/activities/**".to_string(),
                "**".to_string(),
                "!**/*.test.ts".to_string(),
            ],
        };
        let rendered = render_header(&h);
        assert_eq!(
            rendered,
            "---\nversion: 2\nname: cross-cutting-activities-returning-validation-errors-p-fdbb\n\
             description: \"@temporalio/common Library Adoption: Activities Returning Validation \
             Errors Pass Structured\"\nmetadata:\n  actual:\n    \
             adr_id: fdbbcb01-e293-46dd-a7af-ca1f67f66c5b\n    status: accepted\n  scope:\n    \
             root: backend\n    paths:\n      - src/activities/**\n      - \"**\"\n      \
             - \"!**/*.test.ts\"\n---\n"
        );
        assert_round_trips(&rendered, &h);
    }

    #[test]
    fn non_ascii_and_escaped_values_round_trip() {
        let cases: [(&str, &[&str]); 6] = [
            (
                "Caf\u{e9} cr\u{e8}me \u{65e5}\u{672c} \u{1F389}",
                &["src/\u{e9}t\u{e9}/**"],
            ),
            (
                "Tab\there \"quoted\" back\\slash",
                &["a\u{85}b", "nb\u{a0}sp"],
            ),
            (
                "bell\u{07} esc\u{1B} nul\u{0} del\u{7F} bom\u{FEFF}",
                &["\u{2028}\u{2029}"],
            ),
            ("two\nlines\r\n", &["- dash", "key: value", "a #b", "a#b"]),
            ("", &["", " lead", "trail ", "x:"]),
            (
                "true",
                &["1.5", "0x1f", "~", "yes", "2026-11-11", "<<", "12:30"],
            ),
        ];
        for (description, paths) in cases {
            let h = header(description, "./packages/web/", paths);
            assert_round_trips(&render_header(&h), &h);
        }
    }

    #[test]
    fn scalar_quotes_what_js_yaml_quotes() {
        let cases = [
            // Plain.
            ("src/activities/**", "src/activities/**"),
            (".", "."),
            ("accepted", "accepted"),
            ("a:b", "a:b"),
            ("a#b", "a#b"),
            ("x[0],{y}", "x[0],{y}"),
            ("caf\u{e9} \u{1F389}", "caf\u{e9} \u{1F389}"),
            ("_1", "_1"),
            ("+", "+"),
            ("0b", "0b"),
            ("0X1F", "0X1F"),
            ("1e999", "1e999"),
            ("nan", "nan"),
            // Indicators first, or `: ` and ` #` inside.
            ("**", "\"**\""),
            ("!**/*.test.ts", "\"!**/*.test.ts\""),
            ("@scope/pkg", "\"@scope/pkg\""),
            ("-x", "\"-x\""),
            ("a: b", "\"a: b\""),
            ("a #b", "\"a #b\""),
            ("a:", "\"a:\""),
            (" a", "\" a\""),
            ("a ", "\"a \""),
            ("", "\"\""),
            // Other types.
            ("true", "\"true\""),
            ("NULL", "\"NULL\""),
            ("~", "\"~\""),
            ("yes", "\"yes\""),
            ("n", "\"n\""),
            ("1.5", "\"1.5\""),
            (".5", "\".5\""),
            ("0x1f", "\"0x1f\""),
            ("0o17", "\"0o17\""),
            ("0b101", "\"0b101\""),
            ("-0", "\"-0\""),
            ("+1", "\"+1\""),
            ("0", "\"0\""),
            ("01", "\"01\""),
            (".inf", "\".inf\""),
            ("1:20", "\"1:20\""),
            ("2026-11-11", "\"2026-11-11\""),
            ("2026-1-1 1:00:00", "\"2026-1-1 1:00:00\""),
            ("<<", "\"<<\""),
            // Escapes.
            ("a\"b\\c", "a\"b\\c"),
            ("\"q\" \\", "\"\\\"q\\\" \\\\\""),
            ("tab\t", "\"tab\\t\""),
            ("line\nbreak", "\"line\\nbreak\""),
            ("\u{7F}\u{80}\u{1}", "\"\\x7F\\x80\\x01\""),
            ("\u{FEFF}\u{FFFE}", "\"\\uFEFF\\uFFFE\""),
            ("\u{85}\u{A0}\u{2028}\u{2029}", "\"\\N\\_\\L\\P\""),
            (
                "\0\u{7}\u{8}\u{B}\u{C}\r\u{1B}",
                "\"\\0\\a\\b\\v\\f\\r\\e\"",
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(scalar(value), expected, "value {value:?}");
        }
    }

    #[test]
    fn integer_resolver_edges() {
        assert!(is_yaml_int("0"));
        assert!(is_yaml_int("-0"));
        assert!(is_yaml_int("0b1"));
        assert!(!is_yaml_int("0b2"));
        assert!(!is_yaml_int("0x"));
        assert!(!is_yaml_int("-"));
        assert!(!is_yaml_int("1_000"));
        assert!(!is_yaml_int("0a"));
        // Too large for a double: not an integer to js-yaml either.
        assert!(!is_yaml_int(&"9".repeat(400)));
        assert!(is_yaml_float("1.5e3"));
        assert!(!is_yaml_float("1e999"));
        assert!(is_yaml_float("-.Inf"));
    }

    #[test]
    fn description_collapses_trims_and_cuts() {
        assert_eq!(
            description_for("  Use \t Postgres\n\nfor storage  "),
            "Use Postgres for storage"
        );
        // U+FEFF is JavaScript white space; U+0085 is not.
        assert_eq!(
            description_for("a\u{FEFF}b\u{85}c\u{3000}d"),
            "a b\u{85}c d"
        );
        assert_eq!(description_for(""), "");
        assert_eq!(description_for(" \u{A0} "), "");
        let long = "word ".repeat(300);
        let cut = description_for(&long);
        assert_eq!(cut.len(), 1024);
        assert!(cut.ends_with(" word"));
        let exact = "x".repeat(1024);
        assert_eq!(description_for(&exact), exact);
        // A cut landing on a space trims it.
        let spaced = format!("{} tail", "y".repeat(1023));
        assert_eq!(description_for(&spaced), "y".repeat(1023));
        // Astral characters count two units; one straddling the cut is dropped.
        let astral = format!("{}\u{1F389}", "z".repeat(1023));
        assert_eq!(description_for(&astral), "z".repeat(1023));
        let non_ascii = "\u{e9}".repeat(2000);
        assert_eq!(description_for(&non_ascii), "\u{e9}".repeat(1024));
    }

    #[test]
    fn root_is_normalised_as_the_sync_does() {
        assert_eq!(normalize_scope_root(""), ".");
        assert_eq!(normalize_scope_root(" . "), ".");
        assert_eq!(normalize_scope_root("./"), ".");
        assert_eq!(normalize_scope_root("././backend//"), "backend");
        assert_eq!(normalize_scope_root("/"), ".");
        assert_eq!(
            normalize_scope_root("packages/\u{e9}t\u{e9}"),
            "packages/\u{e9}t\u{e9}"
        );
        assert!(render_header(&header("d", "./backend/", &[])).contains("    root: backend\n"));
    }
}
