//! The minisign public key and signature formats, written for a key that never
//! leaves Cloud KMS. Format reference: <https://jedisct1.github.io/minisign/>.
//!
//! A prehashed (`ED`) minisign signature holds two Ed25519 signatures: one over
//! the BLAKE2b-512 digest of the file, and a global one over that signature
//! followed by the trusted comment. KMS signs each message as given (PureEdDSA),
//! so signing a file takes two calls to the key.

use std::io::Read;

use anyhow::{bail, ensure, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use blake2::{Blake2b512, Digest};

/// Something that holds an Ed25519 private key and signs the exact bytes it is given.
pub trait Signer {
    fn sign(&self, message: &[u8]) -> Result<[u8; 64]>;
}

/// A minisign public key: the key id we picked, and the Ed25519 key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicKey {
    pub key_id: [u8; 8],
    pub key: [u8; 32],
}

impl PublicKey {
    /// Parses the two-line `minisign.pub` format.
    pub fn parse(text: &str) -> Result<Self> {
        let mut lines = text.lines();
        let comment = lines.next().unwrap_or_default();
        ensure!(
            comment.starts_with("untrusted comment:"),
            "a minisign public key starts with an `untrusted comment:` line"
        );
        let Some(encoded) = lines.next() else {
            bail!("no key yet, only the comment line");
        };
        let bytes = BASE64
            .decode(encoded.trim())
            .context("the key line isn't base64")?;
        ensure!(
            bytes.len() == 42 && bytes.starts_with(b"Ed"),
            "the key line doesn't hold an Ed25519 minisign key"
        );
        Ok(Self {
            key_id: bytes[2..10].try_into()?,
            key: bytes[10..42].try_into()?,
        })
    }

    /// The key id as `minisign` prints it.
    pub fn key_id_hex(&self) -> String {
        format!("{:X}", u64::from_le_bytes(self.key_id))
    }

    /// The `minisign.pub` file for this key, in the layout `minisign -G` writes.
    pub fn to_file_contents(self) -> String {
        let mut bytes = Vec::with_capacity(42);
        bytes.extend_from_slice(b"Ed");
        bytes.extend_from_slice(&self.key_id);
        bytes.extend_from_slice(&self.key);
        format!(
            "untrusted comment: minisign public key {}\n{}\n",
            self.key_id_hex(),
            BASE64.encode(bytes)
        )
    }
}

/// A public key the repository trusts, by the name its file carries.
#[derive(Clone, Debug)]
pub struct TrustedKey {
    pub name: String,
    pub key: PublicKey,
}

/// The two signatures of a prehashed minisign signature, before a key id is attached.
pub struct Signature {
    signature: [u8; 64],
    trusted_comment: String,
    global_signature: [u8; 64],
}

/// Signs `data` with two calls to `signer`, the way `minisign -S` does with a local key.
pub fn sign(signer: &dyn Signer, data: &[u8], trusted_comment: &str) -> Result<Signature> {
    ensure!(
        !trusted_comment.contains(['\n', '\r']),
        "a trusted comment must fit on one line"
    );
    let signature = signer.sign(&Blake2b512::digest(data))?;
    let mut global = signature.to_vec();
    global.extend_from_slice(trusted_comment.as_bytes());
    let global_signature = signer.sign(&global)?;
    Ok(Signature {
        signature,
        trusted_comment: trusted_comment.to_owned(),
        global_signature,
    })
}

impl Signature {
    /// The `.minisig` file text for this signature under `key_id`.
    pub fn encode(&self, key_id: [u8; 8], untrusted_comment: &str) -> String {
        let mut line = Vec::with_capacity(74);
        line.extend_from_slice(b"ED");
        line.extend_from_slice(&key_id);
        line.extend_from_slice(&self.signature);
        format!(
            "untrusted comment: {untrusted_comment}\n{}\ntrusted comment: {}\n{}\n",
            BASE64.encode(line),
            self.trusted_comment,
            BASE64.encode(self.global_signature)
        )
    }
}

/// Finds the trusted key that made `signature` and returns it with the `.minisig` text.
///
/// The key id isn't covered by either signature, so it can be attached after
/// signing. Each candidate is checked with the minisign-verify crate, an
/// implementation independent of the code above.
pub fn encode_for_trusted_key<'a>(
    signature: &Signature,
    data: &[u8],
    keys: &'a [TrustedKey],
) -> Result<(&'a TrustedKey, String)> {
    for trusted in keys {
        let comment = format!("signature from the actual-cli {} release key", trusted.name);
        let text = signature.encode(trusted.key.key_id, &comment);
        if verify(&text, data, &trusted.key).is_ok() {
            return Ok((trusted, text));
        }
    }
    let names: Vec<&str> = keys.iter().map(|k| k.name.as_str()).collect();
    bail!(
        "the KMS key's signature doesn't verify against the {} key in release-keys/, so it isn't a key this repository trusts",
        names.join(" or ")
    )
}

/// Checks a `.minisig` text over `data` with the minisign-verify crate and returns its trusted comment.
pub fn verify(text: &str, data: &[u8], key: &PublicKey) -> Result<String> {
    let public_key = minisign_verify::PublicKey::decode(&key.to_file_contents())?;
    let signature = minisign_verify::Signature::decode(text)?;
    public_key.verify(data, &signature, false)?;
    Ok(signature.trusted_comment().to_owned())
}

const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The raw Ed25519 key in a `PUBLIC KEY` PEM, the format Cloud KMS exports.
pub fn ed25519_key_from_pem(pem: &str) -> Result<[u8; 32]> {
    let lines: Vec<&str> = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    ensure!(
        lines.len() >= 3
            && lines[0] == "-----BEGIN PUBLIC KEY-----"
            && lines[lines.len() - 1] == "-----END PUBLIC KEY-----",
        "expected a single `PUBLIC KEY` PEM block"
    );
    let der = BASE64
        .decode(lines[1..lines.len() - 1].concat())
        .context("the PEM body isn't base64")?;
    ensure!(
        der.len() == 44 && der[..12] == ED25519_SPKI_PREFIX,
        "the PEM doesn't hold an Ed25519 public key"
    );
    Ok(der[12..].try_into()?)
}

/// Eight random bytes for a new key id.
pub fn random_key_id() -> Result<[u8; 8]> {
    let mut id = [0u8; 8];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut id))
        .context("reading /dev/urandom")?;
    Ok(id)
}

/// Parses a key id written as 16 hex digits, the way `minisign` prints one.
pub fn parse_key_id(hex: &str) -> Result<[u8; 8]> {
    ensure!(
        hex.len() == 16 && hex.chars().all(|c| c.is_ascii_hexdigit()),
        "a key id is 16 hex digits"
    );
    Ok(u64::from_str_radix(hex, 16)?.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::test_support::{trusted, LocalSigner};

    const DATA: &[u8] = b"b5bb9d8014a0f9b1d61e21e796d78dccdf1352f23cd32812f4850b878ae4944c  actual-darwin-arm64.tar.gz\n";
    const COMMENT: &str = "timestamp:1792800000\tfile:SHA256SUMS\ttag:v1.2.3";

    #[test]
    fn public_key_file_round_trips() {
        let key = PublicKey {
            key_id: [1, 2, 3, 4, 5, 6, 7, 8],
            key: [9; 32],
        };
        let text = key.to_file_contents();
        assert!(text.starts_with("untrusted comment: minisign public key 807060504030201\n"));
        assert_eq!(PublicKey::parse(&text).unwrap(), key);
    }

    #[test]
    fn public_key_parse_rejects_what_it_cannot_use() {
        let placeholder = "untrusted comment: placeholder until the setup exports the key\n";
        assert!(PublicKey::parse(placeholder)
            .unwrap_err()
            .to_string()
            .contains("no key yet"));
        assert!(
            PublicKey::parse("RWQBAgMEBQYHCAkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJ\n").is_err()
        );
        assert!(PublicKey::parse("untrusted comment: x\nnot base64!\n").is_err());
        // A signature line is base64 too, but it isn't a key.
        let wrong_length = format!("untrusted comment: x\n{}\n", BASE64.encode([0u8; 74]));
        assert!(PublicKey::parse(&wrong_length).is_err());
        let wrong_algorithm = format!("untrusted comment: x\n{}\n", BASE64.encode([0u8; 42]));
        assert!(PublicKey::parse(&wrong_algorithm).is_err());
    }

    #[test]
    fn signature_verifies_with_minisign_verify_and_names_the_key() {
        let signer = LocalSigner::generate();
        let keys = [trusted("primary", &signer, [7; 8])];
        let signature = sign(&signer, DATA, COMMENT).unwrap();
        let (key, text) = encode_for_trusted_key(&signature, DATA, &keys).unwrap();
        assert_eq!(key.name, "primary");
        assert_eq!(verify(&text, DATA, &key.key).unwrap(), COMMENT);
        assert!(text
            .starts_with("untrusted comment: signature from the actual-cli primary release key\n"));
        assert_eq!(text.lines().count(), 4);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn signature_fails_for_changed_data_or_comment() {
        let signer = LocalSigner::generate();
        let key = trusted("primary", &signer, [7; 8]).key;
        let text = sign(&signer, DATA, COMMENT)
            .unwrap()
            .encode(key.key_id, "x");
        assert!(verify(&text, b"something else\n", &key).is_err());
        let retagged = text.replace("tag:v1.2.3", "tag:v9.9.9");
        assert!(verify(&retagged, DATA, &key).is_err());
    }

    #[test]
    fn the_spare_key_is_found_when_the_primary_did_not_sign() {
        let primary = LocalSigner::generate();
        let spare = LocalSigner::generate();
        let keys = [
            trusted("primary", &primary, [1; 8]),
            trusted("spare", &spare, [2; 8]),
        ];
        let signature = sign(&spare, DATA, COMMENT).unwrap();
        let (key, text) = encode_for_trusted_key(&signature, DATA, &keys).unwrap();
        assert_eq!(key.name, "spare");
        assert!(verify(&text, DATA, &keys[1].key).is_ok());
        assert!(verify(&text, DATA, &keys[0].key).is_err());
    }

    #[test]
    fn a_key_the_repository_does_not_trust_is_refused() {
        let committed = LocalSigner::generate();
        let other = LocalSigner::generate();
        let keys = [trusted("primary", &committed, [1; 8])];
        let signature = sign(&other, DATA, COMMENT).unwrap();
        let err = encode_for_trusted_key(&signature, DATA, &keys).unwrap_err();
        assert!(err
            .to_string()
            .contains("doesn't verify against the primary key"));
    }

    #[test]
    fn a_trusted_comment_must_be_one_line() {
        let signer = LocalSigner::generate();
        assert!(sign(&signer, DATA, "two\nlines").is_err());
        assert!(sign(&signer, DATA, "carriage\rreturn").is_err());
        assert_eq!(signer.calls(), 0);
    }

    #[test]
    fn signing_asks_the_key_twice() {
        let signer = LocalSigner::generate();
        sign(&signer, DATA, COMMENT).unwrap();
        assert_eq!(signer.calls(), 2);
    }

    #[test]
    fn reads_the_ed25519_key_from_a_pem() {
        // The Ed25519 example from RFC 8410, section 10.1.
        let pem = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=\n-----END PUBLIC KEY-----\n";
        let key = ed25519_key_from_pem(pem).unwrap();
        assert_eq!(key[..4], [0x19, 0xbf, 0x44, 0x09]);
        assert_eq!(key[28..], [0x70, 0x31, 0x66, 0xe1]);
    }

    #[test]
    fn rejects_a_pem_that_is_not_one_ed25519_public_key() {
        let p256 = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEp9pP02JNe4KxHFXDP0BmBXxex9gY\nacs72hOnBbmch6927vTJV1iLgGsM1DSXp0TkBLGUG08NTRiG5WKB1WSvQg==\n-----END PUBLIC KEY-----\n";
        assert!(ed25519_key_from_pem(p256)
            .unwrap_err()
            .to_string()
            .contains("doesn't hold an Ed25519"));
        // Right length, but the algorithm is Ed448 (1.3.101.113).
        let mut ed448_oid = ED25519_SPKI_PREFIX.to_vec();
        ed448_oid[8] = 0x71;
        ed448_oid.extend_from_slice(&[0; 32]);
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            BASE64.encode(ed448_oid)
        );
        assert!(ed25519_key_from_pem(&pem).is_err());
        // A valid Ed25519 body under another label.
        let relabelled = "-----BEGIN CERTIFICATE-----\nMCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=\n-----END CERTIFICATE-----\n";
        assert!(ed25519_key_from_pem(relabelled).is_err());
        assert!(ed25519_key_from_pem(
            "-----BEGIN PUBLIC KEY-----\n!!!\n-----END PUBLIC KEY-----\n"
        )
        .is_err());
        assert!(ed25519_key_from_pem("").is_err());
    }

    #[test]
    fn key_ids_parse_from_hex_and_come_out_random() {
        let id = parse_key_id("0807060504030201").unwrap();
        assert_eq!(id, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(parse_key_id("123").is_err());
        assert!(parse_key_id("zz07060504030201").is_err());
        assert_ne!(random_key_id().unwrap(), random_key_id().unwrap());
    }

    /// Stock `minisign -V` is the check a user runs, so the output has to pass it too.
    #[test]
    #[ignore = "needs the minisign CLI on PATH; CI runs it in the release workflow's signing-tool job"]
    fn stock_minisign_accepts_the_signature() {
        let signer = LocalSigner::generate();
        let key = trusted("primary", &signer, [0x5a; 8]).key;
        let text = sign(&signer, DATA, COMMENT)
            .unwrap()
            .encode(key.key_id, "x");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("SHA256SUMS");
        let public_key = dir.path().join("primary.pub");
        std::fs::write(&file, DATA).unwrap();
        std::fs::write(&public_key, key.to_file_contents()).unwrap();
        std::fs::write(dir.path().join("SHA256SUMS.minisig"), &text).unwrap();

        let run = |file: &std::path::Path| {
            Command::new("minisign")
                .arg("-Vm")
                .arg(file)
                .arg("-p")
                .arg(&public_key)
                .output()
                .expect("minisign runs")
        };
        let accepted = run(&file);
        let stdout = String::from_utf8_lossy(&accepted.stdout);
        assert!(
            accepted.status.success(),
            "minisign -V refused: {stdout}{}",
            String::from_utf8_lossy(&accepted.stderr)
        );
        assert!(
            stdout.contains(&format!("Trusted comment: {COMMENT}")),
            "{stdout}"
        );

        // The same check has to fail once the file changes.
        let tampered = dir.path().join("tampered");
        std::fs::write(&tampered, b"something else\n").unwrap();
        std::fs::copy(
            dir.path().join("SHA256SUMS.minisig"),
            dir.path().join("tampered.minisig"),
        )
        .unwrap();
        assert!(!run(&tampered).status.success());
    }
}
