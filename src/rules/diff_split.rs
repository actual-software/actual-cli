//! Cutting a unified diff into parts small enough to judge one at a time.
//!
//! `impl-check` hands the judge a whole diff in one prompt while it fits
//! [`super::check::MAX_JUDGE_PROMPT_BYTES`]. A bigger one is cut here, and
//! [`super::check::check`] judges every part against the same rules and
//! merges the verdicts.

/// Cut `diff` into consecutive parts of at most `budget` bytes.
///
/// A file (the section that starts at a `diff --git ` line) is never cut
/// while it fits, and neighbouring files share a part while there is room. A
/// file larger than `budget` is cut between lines, and every piece of it
/// repeats the file's header (the lines before its first `@@`), so the judge
/// always knows which file a hunk belongs to. A line too long for the room
/// beside that header is cut between characters. The one way a part can run
/// over `budget` is a header longer than half of it: that piece keeps the
/// whole header and half the budget of body.
pub fn split_diff(diff: &str, budget: usize) -> Vec<String> {
    let budget = budget.max(1);
    let mut pieces = Vec::new();
    for file in file_sections(diff) {
        if file.len() <= budget {
            pieces.push(file.to_string());
            continue;
        }
        let (header, body) = file.split_at(header_len(file));
        let room = budget.saturating_sub(header.len()).max(budget / 2);
        for part in pack(split_lines(body, room), room) {
            pieces.push(format!("{header}{part}"));
        }
    }
    pack(pieces, budget)
}

/// `diff` split before every line that opens a file. Text before the first
/// such line is a section of its own.
fn file_sections(diff: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = std::iter::once(0)
        .chain(diff.match_indices("\ndiff --git ").map(|(i, _)| i + 1))
        .collect();
    starts.push(diff.len());
    starts
        .windows(2)
        .map(|w| &diff[w[0]..w[1]])
        .filter(|section| !section.is_empty())
        .collect()
}

/// Bytes of `file` before its first hunk line. Zero when it has none, since
/// then there is no body to tell apart from a header.
fn header_len(file: &str) -> usize {
    if file.starts_with("@@") {
        return 0;
    }
    file.find("\n@@").map_or(0, |i| i + 1)
}

/// `text` split after every newline, with a line longer than `room` cut into
/// pieces of at most `room` bytes on character boundaries (or one character,
/// when `room` is narrower than the character).
fn split_lines(text: &str, room: usize) -> Vec<&str> {
    let mut out = Vec::new();
    for line in text.split_inclusive('\n') {
        let mut rest = line;
        while rest.len() > room {
            let mut cut = room;
            while !rest.is_char_boundary(cut) {
                cut -= 1;
            }
            if cut == 0 {
                cut = rest.chars().next().map_or(rest.len(), char::len_utf8);
            }
            let (head, tail) = rest.split_at(cut);
            out.push(head);
            rest = tail;
        }
        if !rest.is_empty() {
            out.push(rest);
        }
    }
    out
}

/// Join consecutive `units` while each part stays within `room`. A unit that
/// is bigger than `room` on its own gets a part to itself.
fn pack<S: AsRef<str>>(units: impl IntoIterator<Item = S>, room: usize) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for unit in units {
        let unit = unit.as_ref();
        match parts.last_mut() {
            Some(last) if last.len() + unit.len() <= room => last.push_str(unit),
            _ => parts.push(unit.to_string()),
        }
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One file's section of a unified diff, with `lines` added lines.
    fn file_diff(path: &str, lines: usize) -> String {
        let mut out = format!(
            "diff --git a/{path} b/{path}\nindex 0000000..1111111 100644\n--- a/{path}\n+++ b/{path}\n@@ -0,0 +1,{lines} @@\n"
        );
        for i in 0..lines {
            out.push_str(&format!("+line {i} of {path}\n"));
        }
        out
    }

    #[test]
    fn test_a_diff_within_the_budget_is_one_part() {
        let diff = format!("{}{}", file_diff("a.rs", 3), file_diff("b.rs", 3));
        assert_eq!(split_diff(&diff, diff.len()), vec![diff]);
    }

    /// Files are never cut while they fit, and whole files share a part
    /// while there is room, so the parts together are the diff, in order.
    #[test]
    fn test_whole_files_are_packed_into_parts_in_order() {
        let files: Vec<String> = (0..6).map(|i| file_diff(&format!("f{i}.rs"), 20)).collect();
        let diff = files.concat();
        let budget = files[0].len() * 2 + 10;

        let parts = split_diff(&diff, budget);

        assert_eq!(parts.concat(), diff);
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert!(part.len() <= budget, "{} > {budget}", part.len());
            assert_eq!(part.matches("diff --git ").count(), 2, "{part}");
        }
    }

    /// A file bigger than the budget is cut between lines, and every piece
    /// repeats its header so each part still names the file.
    #[test]
    fn test_a_file_over_the_budget_is_cut_between_lines_under_its_header() {
        let big = file_diff("big.rs", 400);
        let header = &big[..header_len(&big)];
        assert!(header.starts_with("diff --git a/big.rs"));
        assert!(header.ends_with("+++ b/big.rs\n"));
        let budget = big.len() / 4;

        let parts = split_diff(&big, budget);

        assert!(parts.len() >= 4, "{}", parts.len());
        let mut body = String::new();
        for part in &parts {
            assert!(part.len() <= budget, "{} > {budget}", part.len());
            assert!(part.starts_with(header), "{part}");
            assert!(part.ends_with('\n'), "a part ends mid-line: {part:?}");
            body.push_str(&part[header.len()..]);
        }
        assert_eq!(body, big[header.len()..]);
    }

    /// Text before the first file header (a `git show` commit message, say)
    /// is kept, and a section with no hunk line is cut as plain lines.
    #[test]
    fn test_a_preamble_and_a_section_without_hunks_are_kept_whole() {
        let preamble = "commit 0123\nAuthor: someone\n\n    a message\n\n".repeat(40);
        let diff = format!("{preamble}{}", file_diff("a.rs", 2));
        let budget = preamble.len() / 3;

        let parts = split_diff(&diff, budget);

        assert_eq!(parts.concat(), diff);
        assert!(parts.iter().all(|p| p.len() <= budget));
        assert!(parts.last().unwrap().contains("diff --git a/a.rs"));
    }

    /// A single line too long for any part is cut between characters, never
    /// inside one.
    #[test]
    fn test_a_line_longer_than_the_room_is_cut_on_character_boundaries() {
        let line = format!("+{}\n", "é".repeat(50));
        let pieces = split_lines(&line, 9);
        assert!(pieces.len() > 1);
        assert!(pieces.iter().all(|p| p.len() <= 9));
        assert_eq!(pieces.concat(), line);
    }

    /// Room narrower than one character still moves forward a character at
    /// a time rather than looping on a zero-width cut.
    #[test]
    fn test_room_narrower_than_a_character_takes_one_character() {
        let pieces = split_lines("éé", 1);
        assert_eq!(pieces, vec!["é", "é"]);
    }

    /// A header longer than half the budget keeps half the budget for body,
    /// rather than leaving none and cutting the body into slivers.
    #[test]
    fn test_a_header_over_half_the_budget_still_leaves_room_for_body() {
        let long_path = "p".repeat(300);
        let big = file_diff(&long_path, 50);
        let header = header_len(&big);
        let budget = header + 40;

        let parts = split_diff(&big, budget);

        assert!(parts.iter().all(|p| p.len() <= header + budget / 2));
        let body: String = parts.iter().map(|p| &p[header..]).collect();
        assert_eq!(body, big[header..]);
    }

    #[test]
    fn test_header_len_is_zero_without_a_hunk_or_at_a_bare_hunk() {
        assert_eq!(header_len("no hunks here\n"), 0);
        assert_eq!(header_len("@@ -1 +1 @@\n+x\n"), 0);
        assert_eq!(header_len("diff --git a/x b/x\n@@ -1 +1 @@\n"), 19);
    }

    #[test]
    fn test_an_empty_diff_has_no_parts() {
        assert!(split_diff("", 100).is_empty());
    }

    /// A zero budget is treated as one byte rather than dividing nothing.
    #[test]
    fn test_a_zero_budget_still_returns_the_whole_diff() {
        assert_eq!(split_diff("ab", 0).concat(), "ab");
    }
}
