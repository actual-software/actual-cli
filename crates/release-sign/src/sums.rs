//! SHA256SUMS in the format `sha256sum` writes: a lowercase hex digest, two
//! spaces and a file name, one line per file.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};

pub const FILE_NAME: &str = "SHA256SUMS";

#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub sha256: String,
    pub name: String,
}

/// Parses a SHA256SUMS file, refusing anything `sha256sum` wouldn't have written for plain files.
pub fn parse(text: &str) -> Result<Vec<Entry>> {
    ensure!(
        !text.is_empty() && text.ends_with('\n') && !text.contains('\r'),
        "{FILE_NAME} is empty, doesn't end with a newline, or has Windows line endings"
    );
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        let Some((sha256, name)) = line.split_once("  ") else {
            bail!("{FILE_NAME} line {line_no} isn't `<sha256>  <file name>`");
        };
        ensure!(
            sha256.len() == 64
                && sha256
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{FILE_NAME} line {line_no} doesn't start with a lowercase hex SHA-256"
        );
        ensure!(
            is_plain_file_name(name),
            "{FILE_NAME} line {line_no} names `{name}`, which isn't a plain file name"
        );
        ensure!(
            names.insert(name.to_owned()),
            "{FILE_NAME} lists `{name}` twice"
        );
        entries.push(Entry {
            sha256: sha256.to_owned(),
            name: name.to_owned(),
        });
    }
    Ok(entries)
}

fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Checks that `dir` holds every file `entries` lists, with the listed digest,
/// and nothing else apart from the names in `also_allowed`.
pub fn check_dir(dir: &Path, entries: &[Entry], also_allowed: &[&str]) -> Result<()> {
    let mut present = BTreeSet::new();
    for item in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let item = item?;
        ensure!(
            item.file_type()?.is_file(),
            "{} holds `{}`, which isn't a file",
            dir.display(),
            item.file_name().to_string_lossy()
        );
        present.insert(item.file_name().to_string_lossy().into_owned());
    }
    let listed: BTreeSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let unexpected: Vec<&str> = present
        .iter()
        .map(String::as_str)
        .filter(|name| !listed.contains(name) && !also_allowed.contains(name))
        .collect();
    ensure!(
        unexpected.is_empty(),
        "found files {FILE_NAME} doesn't list: {}",
        unexpected.join(", ")
    );
    for entry in entries {
        ensure!(
            present.contains(&entry.name),
            "{} is listed in {FILE_NAME} but missing",
            entry.name
        );
        let actual = sha256_file(&dir.join(&entry.name))?;
        ensure!(
            actual == entry.sha256,
            "{} has SHA-256 {actual}, but {FILE_NAME} lists {}",
            entry.name,
            entry.sha256
        );
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn sums_for(files: &[(&str, &str)]) -> String {
        files
            .iter()
            .map(|(sha, name)| format!("{sha}  {name}\n"))
            .collect()
    }

    #[test]
    fn parses_what_sha256sum_writes() {
        let text = sums_for(&[
            (ABC_SHA256, "actual-linux-x64.tar.gz"),
            (EMPTY_SHA256, "empty"),
        ]);
        let entries = parse(&text).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "actual-linux-x64.tar.gz");
        assert_eq!(entries[0].sha256, ABC_SHA256);
    }

    #[test]
    fn refuses_anything_else() {
        for (text, why) in [
            (String::new(), "empty"),
            (format!("{ABC_SHA256}  a"), "no trailing newline"),
            (format!("{ABC_SHA256} a\n"), "one space"),
            (format!("{ABC_SHA256}  *a\n"), "binary-mode marker"),
            (
                format!("{}  a\n", ABC_SHA256.to_uppercase()),
                "uppercase digest",
            ),
            (format!("{}  a\n", &ABC_SHA256[1..]), "short digest"),
            (format!("{ABC_SHA256}  ../a\n"), "path"),
            (format!("{ABC_SHA256}  .hidden\n"), "dot file"),
            (format!("{ABC_SHA256}  a b\n"), "space in name"),
            (format!("{ABC_SHA256}  a\n\n"), "blank line"),
            (format!("{ABC_SHA256}  a\r\n"), "carriage return"),
            (
                sums_for(&[(ABC_SHA256, "a"), (EMPTY_SHA256, "a")]),
                "duplicate",
            ),
        ] {
            assert!(parse(&text).is_err(), "accepted a SHA256SUMS with {why}");
        }
    }

    fn dir_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            fs::write(dir.path().join(name), content).unwrap();
        }
        dir
    }

    #[test]
    fn a_directory_matching_its_sums_passes() {
        let entries = parse(&sums_for(&[
            (ABC_SHA256, "a.tar.gz"),
            (EMPTY_SHA256, "b.tar.gz"),
        ]))
        .unwrap();
        let dir = dir_with(&[
            ("a.tar.gz", b"abc"),
            ("b.tar.gz", b""),
            ("SHA256SUMS", b"x"),
        ]);
        check_dir(dir.path(), &entries, &[FILE_NAME]).unwrap();
    }

    #[test]
    fn a_changed_file_fails() {
        let entries = parse(&sums_for(&[(ABC_SHA256, "a.tar.gz")])).unwrap();
        let dir = dir_with(&[("a.tar.gz", b"abd")]);
        let err = check_dir(dir.path(), &entries, &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("a.tar.gz has SHA-256"), "{err}");
    }

    #[test]
    fn a_missing_file_fails() {
        let entries = parse(&sums_for(&[
            (ABC_SHA256, "a.tar.gz"),
            (EMPTY_SHA256, "b.tar.gz"),
        ]))
        .unwrap();
        let dir = dir_with(&[("a.tar.gz", b"abc")]);
        let err = check_dir(dir.path(), &entries, &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("b.tar.gz is listed"), "{err}");
    }

    #[test]
    fn an_unlisted_file_fails_unless_allowed() {
        let entries = parse(&sums_for(&[(ABC_SHA256, "a.tar.gz")])).unwrap();
        let dir = dir_with(&[("a.tar.gz", b"abc"), ("extra.tar.gz", b"")]);
        let err = check_dir(dir.path(), &entries, &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("extra.tar.gz"), "{err}");
        check_dir(dir.path(), &entries, &["extra.tar.gz"]).unwrap();
    }

    #[test]
    fn a_subdirectory_fails() {
        let entries = parse(&sums_for(&[(ABC_SHA256, "a.tar.gz")])).unwrap();
        let dir = dir_with(&[("a.tar.gz", b"abc")]);
        fs::create_dir(dir.path().join("nested")).unwrap();
        assert!(check_dir(dir.path(), &entries, &[]).is_err());
    }

    #[test]
    fn hashes_a_file_larger_than_one_read() {
        let content = vec![b'x'; (1 << 16) + 3];
        let dir = dir_with(&[("big", &content)]);
        let expected = format!("{:x}", Sha256::digest(&content));
        assert_eq!(sha256_file(&dir.path().join("big")).unwrap(), expected);
        assert!(sha256_file(&dir.path().join("absent")).is_err());
        assert!(check_dir(&dir.path().join("absent"), &[], &[]).is_err());
    }
}
