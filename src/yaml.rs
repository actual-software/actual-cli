//! YAML reading and writing, with the parser options kept in one place.
//!
//! The readers split on who wrote the input: [`from_str`] for the files the
//! CLI writes itself, [`from_repo_str`] for files in the repository under
//! analysis.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Parses YAML the CLI wrote itself: its config file, stored credentials and
/// caches.
///
/// The parse budget is off because the config file carries the analysis and
/// tailoring caches, each allowed up to
/// [`CACHE_MAX_SIZE_BYTES`](crate::config::types::CACHE_MAX_SIZE_BYTES)
/// serialized, which can pass serde-saphyr's default node and event limits.
/// Snippets are off because these files hold tokens and API keys, and a
/// snippet quotes the lines around an error into its message. Non-finite
/// floats reach a `serde_json::Value` as null instead of failing the whole
/// document, so the config's lenient cache fields stay lenient.
pub(crate) fn from_str<T: DeserializeOwned>(input: &str) -> Result<T, serde_saphyr::Error> {
    serde_saphyr::from_str_with_options(
        input,
        serde_saphyr::options! {
            budget: None,
            with_snippet: false,
            non_finite_float_policy: serde_saphyr::NonFiniteFloatPolicy::PassThrough,
        },
    )
}

/// Parses YAML read from the repository under analysis, such as
/// `pnpm-workspace.yaml`, under serde-saphyr's default parse budget.
pub(crate) fn from_repo_str<T: DeserializeOwned>(input: &str) -> Result<T, serde_saphyr::Error> {
    serde_saphyr::from_str_with_options(
        input,
        serde_saphyr::options! {
            with_snippet: false,
        },
    )
}

/// Serializes a value as YAML, keeping every scalar on its key's line.
///
/// Block scalars are off because `actual config show` redacts secrets line by
/// line, and a long API key written as a folded block would carry its value
/// onto the next line, past the redaction.
pub(crate) fn to_string<T: Serialize>(value: &T) -> Result<String, serde_saphyr::SerializeError> {
    serde_saphyr::to_string_with_options(
        value,
        serde_saphyr::ser_options! {
            prefer_block_scalars: false,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// More nodes than serde-saphyr's default budget allows (250,000).
    fn over_default_budget() -> String {
        to_string(&vec![0u8; 300_000]).expect("serialize")
    }

    #[test]
    fn test_own_files_parse_past_the_default_budget() {
        let parsed: Vec<u8> = from_str(&over_default_budget()).expect("no budget on own files");
        assert_eq!(parsed.len(), 300_000);
    }

    #[test]
    fn test_repo_files_keep_the_default_budget() {
        let err = from_repo_str::<Vec<u8>>(&over_default_budget()).unwrap_err();
        assert!(
            err.to_string().contains("budget"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_errors_do_not_quote_neighbouring_lines() {
        let yaml = "access_token: secret-access-value\norganization_id: [1, 2]\n";
        let own = from_str::<crate::auth::store::StoredCredentials>(yaml).unwrap_err();
        let repo = from_repo_str::<crate::auth::store::StoredCredentials>(yaml).unwrap_err();
        for err in [own.to_string(), repo.to_string()] {
            assert!(
                !err.contains("secret-access-value"),
                "error quotes a secret: {err}"
            );
        }
    }

    #[test]
    fn test_non_finite_floats_reach_json_values_as_null() {
        let value: serde_json::Value = from_str("a: .nan\nb: .inf\nc: 1.5\n").expect("parse");
        assert_eq!(value, serde_json::json!({ "a": null, "b": null, "c": 1.5 }));
    }

    #[test]
    fn test_scalars_stay_on_one_line() {
        let long = "x".repeat(300);
        let value =
            std::collections::BTreeMap::from([("long", long.as_str()), ("multi", "a\nb\n")]);
        let yaml = to_string(&value).expect("serialize");
        assert_eq!(yaml.lines().count(), 2, "got:\n{yaml}");
        let back: std::collections::BTreeMap<String, String> = from_str(&yaml).expect("parse");
        assert_eq!(back["long"], long);
        assert_eq!(back["multi"], "a\nb\n");
    }

    #[test]
    fn test_round_trip() {
        let value = std::collections::BTreeMap::from([("key", vec!["a", "b: c"])]);
        let yaml = to_string(&value).expect("serialize");
        let back: std::collections::BTreeMap<String, Vec<String>> = from_str(&yaml).expect("parse");
        assert_eq!(back["key"], ["a", "b: c"]);
    }
}
