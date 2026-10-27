//! Checks a release the way docs/RELEASE_SIGNING.md tells a person to:
//! `SHA256SUMS.minisig` against a key committed in `release-keys/` first,
//! then an archive against the SHA-256 that `SHA256SUMS` lists for it.

use std::path::Path;

use sha2::{Digest, Sha256};

/// The list of every archive's SHA-256 that a release publishes.
pub const SUMS_FILE: &str = "SHA256SUMS";

/// The minisign signature over [`SUMS_FILE`].
pub const SIGNATURE_FILE: &str = "SHA256SUMS.minisig";

const COMMITTED_KEYS: [(&str, &str); 2] = [
    ("primary", include_str!("../release-keys/primary.pub")),
    ("spare", include_str!("../release-keys/spare.pub")),
];

/// A public key a release signature may come from, named after its file in
/// `release-keys/`.
pub struct ReleaseKey {
    name: String,
    key: minisign_verify::PublicKey,
}

impl ReleaseKey {
    /// Read a key in the two-line format `minisign -G` writes.
    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        let key = minisign_verify::PublicKey::decode(text)
            .map_err(|e| format!("release-keys/{name}.pub isn't a minisign public key: {e}"))?;
        Ok(Self {
            name: name.to_string(),
            key,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The keys committed in `release-keys/`: the primary, which signs releases,
/// and the spare that takes over if the primary is ever retired.
pub fn committed_keys() -> Result<Vec<ReleaseKey>, String> {
    COMMITTED_KEYS
        .iter()
        .map(|(name, text)| ReleaseKey::parse(name, text))
        .collect()
}

/// Check that `signature`, the text of a `.minisig` file, signs `sums` with
/// one of `keys`, and that its trusted comment names [`SUMS_FILE`] and `tag`.
/// Returns the key that made it.
pub fn verify_sums<'k>(
    sums: &[u8],
    signature: &str,
    tag: &str,
    keys: &'k [ReleaseKey],
) -> Result<&'k ReleaseKey, String> {
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|e| format!("{SIGNATURE_FILE} isn't a minisign signature: {e}"))?;
    let key = keys
        .iter()
        .find(|key| key.key.verify(sums, &signature, false).is_ok())
        .ok_or_else(|| {
            format!("{SIGNATURE_FILE} doesn't verify against the release keys built into actual")
        })?;
    check_trusted_comment(signature.trusted_comment(), tag)?;
    Ok(key)
}

/// The signed comment reads `timestamp:<seconds>\tfile:SHA256SUMS\ttag:<tag>`.
/// An older release's signature still verifies, so its `file:` and `tag:`
/// fields must each appear once and match exactly.
fn check_trusted_comment(comment: &str, tag: &str) -> Result<(), String> {
    let fields: Vec<&str> = comment.split('\t').collect();
    let only = |prefix: &str| match fields
        .iter()
        .filter_map(|field| field.strip_prefix(prefix))
        .collect::<Vec<_>>()[..]
    {
        [value] => Some(value),
        _ => None,
    };
    if only("file:") == Some(SUMS_FILE) && only("tag:") == Some(tag) {
        return Ok(());
    }
    Err(format!(
        "The signature on {SUMS_FILE} is for another release (its signed comment reads {comment:?}), so it doesn't vouch for {tag}"
    ))
}

/// The SHA-256 that `sums` lists for `file`. `sums` is in the format
/// `sha256sum` writes: a lowercase hex digest, two spaces and a file name on
/// each line.
pub fn listed_sha256<'s>(sums: &'s str, file: &str) -> Result<&'s str, String> {
    let mut listed = None;
    for line in sums.lines() {
        let Some((digest, name)) = line.split_once("  ") else {
            return Err(format!(
                "{SUMS_FILE} has a line that isn't `<sha256>  <file>`: {line:?}"
            ));
        };
        if name == file && listed.replace(digest).is_some() {
            return Err(format!("{SUMS_FILE} lists {file} twice"));
        }
    }
    let digest = listed.ok_or_else(|| format!("{SUMS_FILE} doesn't list {file}"))?;
    if digest.len() == 64
        && digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Ok(digest);
    }
    Err(format!(
        "{SUMS_FILE} lists {file} with {digest:?}, which isn't a SHA-256"
    ))
}

/// Check the archive at `path` against the SHA-256 that `sums` lists for
/// `file`.
pub fn check_archive(sums: &str, file: &str, path: &Path) -> Result<(), String> {
    let listed = listed_sha256(sums, file)?;
    let actual = sha256_of(path).map_err(|e| format!("Couldn't read {file}: {e}"))?;
    if actual != listed {
        return Err(format!(
            "{file} has SHA-256 {actual}, but {SUMS_FILE} lists {listed}"
        ));
    }
    Ok(())
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut hasher = Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// A minisign key made fresh on every test run, so no private key is ever
/// committed to this public repository.
#[cfg(test)]
pub(crate) mod test_key {
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;
    use blake2::{Blake2b512, Digest};
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::ReleaseKey;

    pub(crate) struct TestKey {
        pair: Ed25519KeyPair,
        id: [u8; 8],
    }

    impl TestKey {
        pub(crate) fn generate(id: u8) -> Self {
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
            Self {
                pair: Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap(),
                id: [id; 8],
            }
        }

        /// The public key in the format `minisign -G` writes.
        pub(crate) fn public_file(&self) -> String {
            let mut bytes = b"Ed".to_vec();
            bytes.extend_from_slice(&self.id);
            bytes.extend_from_slice(self.pair.public_key().as_ref());
            format!(
                "untrusted comment: minisign public key\n{}\n",
                BASE64.encode(bytes)
            )
        }

        pub(crate) fn release_key(&self, name: &str) -> ReleaseKey {
            ReleaseKey::parse(name, &self.public_file()).unwrap()
        }

        /// A prehashed signature, the kind `minisign -S` and the release
        /// signing tool make.
        pub(crate) fn sign(&self, data: &[u8], trusted_comment: &str) -> String {
            self.encode(b"ED", &Blake2b512::digest(data), trusted_comment)
        }

        /// A signature in the legacy format minisign made before version 0.8,
        /// over the data itself.
        pub(crate) fn sign_legacy(&self, data: &[u8], trusted_comment: &str) -> String {
            self.encode(b"Ed", data, trusted_comment)
        }

        fn encode(&self, algorithm: &[u8], message: &[u8], trusted_comment: &str) -> String {
            let signature = self.pair.sign(message);
            let mut line = algorithm.to_vec();
            line.extend_from_slice(&self.id);
            line.extend_from_slice(signature.as_ref());
            let mut global = signature.as_ref().to_vec();
            global.extend_from_slice(trusted_comment.as_bytes());
            format!(
                "untrusted comment: test signature\n{}\ntrusted comment: {trusted_comment}\n{}\n",
                BASE64.encode(line),
                BASE64.encode(self.pair.sign(&global))
            )
        }
    }

    /// The trusted comment the release signing tool writes for `tag`.
    pub(crate) fn comment(tag: &str) -> String {
        format!("timestamp:1792800000\tfile:SHA256SUMS\ttag:{tag}")
    }
}

#[cfg(test)]
mod tests {
    use super::test_key::{comment, TestKey};
    use super::*;

    const SUMS: &[u8] = b"5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03  actual-darwin-arm64.tar.gz\n2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae  actual-linux-x64.tar.gz\n";

    fn keys(primary: &TestKey, spare: &TestKey) -> Vec<ReleaseKey> {
        vec![primary.release_key("primary"), spare.release_key("spare")]
    }

    // ---- keys ----

    #[test]
    fn the_committed_keys_parse_in_order() {
        let keys = committed_keys().unwrap();
        let names: Vec<&str> = keys.iter().map(ReleaseKey::name).collect();
        assert_eq!(names, ["primary", "spare"]);
    }

    #[test]
    fn a_key_that_does_not_parse_names_its_file() {
        let err = ReleaseKey::parse("primary", "untrusted comment: placeholder\n")
            .err()
            .unwrap();
        assert!(
            err.starts_with("release-keys/primary.pub isn't a minisign public key"),
            "{err}"
        );
    }

    #[test]
    fn a_throwaway_signature_does_not_verify_against_the_committed_keys() {
        let key = TestKey::generate(1);
        let signature = key.sign(SUMS, &comment("v1.2.3"));
        let err = verify_sums(SUMS, &signature, "v1.2.3", &committed_keys().unwrap())
            .err()
            .unwrap();
        assert!(
            err.contains("doesn't verify against the release keys"),
            "{err}"
        );
    }

    // ---- the signature ----

    #[test]
    fn verify_sums_names_the_key_that_signed() {
        let primary = TestKey::generate(1);
        let spare = TestKey::generate(2);
        let keys = keys(&primary, &spare);

        let by_primary = primary.sign(SUMS, &comment("v1.2.3"));
        assert_eq!(
            verify_sums(SUMS, &by_primary, "v1.2.3", &keys)
                .unwrap()
                .name(),
            "primary"
        );
        let by_spare = spare.sign(SUMS, &comment("v1.2.3"));
        assert_eq!(
            verify_sums(SUMS, &by_spare, "v1.2.3", &keys)
                .unwrap()
                .name(),
            "spare"
        );
    }

    #[test]
    fn verify_sums_refuses_changed_data_an_unknown_key_or_garbage() {
        let primary = TestKey::generate(1);
        let spare = TestKey::generate(2);
        let keys = keys(&primary, &spare);
        let signature = primary.sign(SUMS, &comment("v1.2.3"));

        let mut changed = SUMS.to_vec();
        changed[0] = b'6';
        assert!(verify_sums(&changed, &signature, "v1.2.3", &keys)
            .err()
            .unwrap()
            .contains("doesn't verify"));

        // Same key id as the primary, different key.
        let impostor = TestKey::generate(1).sign(SUMS, &comment("v1.2.3"));
        assert!(verify_sums(SUMS, &impostor, "v1.2.3", &keys).is_err());

        let err = verify_sums(SUMS, "not a signature", "v1.2.3", &keys)
            .err()
            .unwrap();
        assert!(
            err.starts_with("SHA256SUMS.minisig isn't a minisign signature"),
            "{err}"
        );
    }

    #[test]
    fn verify_sums_refuses_the_legacy_format() {
        let primary = TestKey::generate(1);
        let keys = keys(&primary, &TestKey::generate(2));
        let legacy = primary.sign_legacy(SUMS, &comment("v1.2.3"));
        assert!(verify_sums(SUMS, &legacy, "v1.2.3", &keys).is_err());
    }

    #[test]
    fn verify_sums_refuses_a_signed_comment_that_has_been_edited() {
        let primary = TestKey::generate(1);
        let keys = keys(&primary, &TestKey::generate(2));
        let signature = primary
            .sign(SUMS, &comment("v1.2.2"))
            .replace("tag:v1.2.2", "tag:v1.2.3");
        assert!(verify_sums(SUMS, &signature, "v1.2.3", &keys).is_err());
    }

    // ---- the trusted comment ----

    #[test]
    fn the_signed_comment_must_name_this_tag_exactly() {
        let primary = TestKey::generate(1);
        let keys = keys(&primary, &TestKey::generate(2));
        let check = |comment: &str| {
            verify_sums(SUMS, &primary.sign(SUMS, comment), "v1.2.3", &keys).map(|_| ())
        };

        assert_eq!(check("timestamp:1\tfile:SHA256SUMS\ttag:v1.2.3"), Ok(()));
        // A field the format gains later doesn't matter.
        assert_eq!(
            check("timestamp:1\tfile:SHA256SUMS\ttag:v1.2.3\tsigner:primary"),
            Ok(())
        );

        let err = check("timestamp:1\tfile:SHA256SUMS\ttag:v1.2.2").unwrap_err();
        assert!(
            err.contains("is for another release")
                && err.contains("tag:v1.2.2")
                && err.ends_with("doesn't vouch for v1.2.3"),
            "{err}"
        );
        for refused in [
            "timestamp:1\tfile:SHA256SUMS\ttag:v1.2.30",
            "timestamp:1\tfile:SHA256SUMS\ttag:v1.2.3 ",
            "timestamp:1\tfile:SHA256SUMS tag:v1.2.3",
            "timestamp:1\tfile:SHA256SUMS\ttag:v1.2.3\ttag:v1.2.2",
            "timestamp:1\tfile:other\ttag:v1.2.3",
            "timestamp:1\ttag:v1.2.3",
            "file:SHA256SUMS\tfile:SHA256SUMS\ttag:v1.2.3",
        ] {
            assert!(check(refused).is_err(), "{refused:?}");
        }
    }

    // ---- SHA256SUMS ----

    #[test]
    fn listed_sha256_finds_the_file() {
        let sums = std::str::from_utf8(SUMS).unwrap();
        assert_eq!(
            listed_sha256(sums, "actual-linux-x64.tar.gz"),
            Ok("2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae")
        );
        assert_eq!(
            listed_sha256(sums, "actual-linux-arm64.tar.gz"),
            Err("SHA256SUMS doesn't list actual-linux-arm64.tar.gz".to_string())
        );
    }

    #[test]
    fn listed_sha256_refuses_what_sha256sum_would_not_write() {
        let digest = "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae";
        let twice = format!("{digest}  a.tar.gz\n{digest}  a.tar.gz\n");
        assert_eq!(
            listed_sha256(&twice, "a.tar.gz"),
            Err("SHA256SUMS lists a.tar.gz twice".to_string())
        );
        let malformed = format!("{digest} a.tar.gz\n");
        assert!(listed_sha256(&malformed, "a.tar.gz")
            .unwrap_err()
            .starts_with("SHA256SUMS has a line that isn't"));
        for digest in [
            digest.to_uppercase(),
            digest[1..].to_string(),
            "z".repeat(64),
        ] {
            let sums = format!("{digest}  a.tar.gz\n");
            assert!(
                listed_sha256(&sums, "a.tar.gz")
                    .unwrap_err()
                    .ends_with("which isn't a SHA-256"),
                "{digest}"
            );
        }
    }

    #[test]
    fn check_archive_compares_the_file_with_its_listed_digest() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("actual-linux-x64.tar.gz");
        std::fs::write(&archive, b"foo").unwrap();
        let sums = std::str::from_utf8(SUMS).unwrap();
        assert_eq!(
            check_archive(sums, "actual-linux-x64.tar.gz", &archive),
            Ok(())
        );

        std::fs::write(&archive, b"bar").unwrap();
        let err = check_archive(sums, "actual-linux-x64.tar.gz", &archive).unwrap_err();
        assert!(
            err.starts_with("actual-linux-x64.tar.gz has SHA-256 fcde2b2e")
                && err.ends_with("but SHA256SUMS lists 2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae"),
            "{err}"
        );

        assert!(check_archive(sums, "actual-linux-arm64.tar.gz", &archive).is_err());
        let missing = dir.path().join("missing");
        assert!(check_archive(sums, "actual-linux-x64.tar.gz", &missing)
            .unwrap_err()
            .starts_with("Couldn't read actual-linux-x64.tar.gz"));
    }
}
