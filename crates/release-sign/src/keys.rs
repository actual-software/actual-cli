//! The public keys committed in `release-keys/`, built into the tool so it
//! checks every signature against exactly what the repository publishes.

use anyhow::{Context, Result};

use crate::minisign::{PublicKey, TrustedKey};

const COMMITTED: [(&str, &str); 2] = [
    ("primary", include_str!("../../../release-keys/primary.pub")),
    ("spare", include_str!("../../../release-keys/spare.pub")),
];

pub fn committed() -> Result<Vec<TrustedKey>> {
    parse_all(&COMMITTED)
}

fn parse_all(files: &[(&str, &str)]) -> Result<Vec<TrustedKey>> {
    files
        .iter()
        .map(|(name, text)| {
            let key = PublicKey::parse(text).with_context(|| {
                format!("release-keys/{name}.pub can't be used (the one-time setup of the signing keys, in the team's internal docs, writes it)")
            })?;
            Ok(TrustedKey {
                name: (*name).to_owned(),
                key,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placeholder_key_names_the_file_and_the_setup() {
        let err = parse_all(&[("primary", "untrusted comment: placeholder\n")]).unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("release-keys/primary.pub")
                && text.contains(
                    "the one-time setup of the signing keys, in the team's internal docs"
                ),
            "{text}"
        );
    }

    #[test]
    fn real_keys_parse_in_order() {
        let primary = PublicKey {
            key_id: [1; 8],
            key: [2; 32],
        };
        let spare = PublicKey {
            key_id: [3; 8],
            key: [4; 32],
        };
        let files = [
            ("primary", primary.to_file_contents()),
            ("spare", spare.to_file_contents()),
        ];
        let borrowed: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
        let keys = parse_all(&borrowed).unwrap();
        assert_eq!(keys[0].name, "primary");
        assert_eq!(keys[0].key, primary);
        assert_eq!(keys[1].key, spare);
    }

    #[test]
    fn the_committed_files_are_placeholders_or_real_keys() {
        for (name, text) in COMMITTED {
            let first = text.lines().next().unwrap_or_default();
            assert!(
                first.starts_with("untrusted comment:"),
                "release-keys/{name}.pub"
            );
            if text.lines().count() > 1 {
                PublicKey::parse(text).unwrap();
            }
        }
    }
}
