use std::path::Path;

use sha2::{Digest, Sha256};

use crate::config::paths::{load_from, save_to};
use crate::config::types::{CachedTailoring, CACHE_MAX_SIZE_BYTES};
use crate::tailoring::types::TailoringOutput;

/// Return the pipeline skip message for a tailoring cache hit.
pub(crate) fn tailoring_cache_skip_msg(applicable: usize) -> &'static str {
    if applicable == 0 {
        "No ADRs applicable to this project (cached)"
    } else {
        "Using cached tailoring result"
    }
}

pub(crate) fn compute_repo_key(root_dir: &Path) -> String {
    compute_repo_key_with_timeout(root_dir, std::time::Duration::from_secs(5))
}

pub(crate) fn compute_repo_key_with_timeout(
    root_dir: &Path,
    timeout: std::time::Duration,
) -> String {
    let root_dir_buf = root_dir.to_path_buf();

    // Use tokio::process::Command with kill_on_drop(true) so that when the
    // timeout fires the child is sent SIGKILL rather than left as an orphan.
    // A single-threaded runtime is used to keep this function synchronous.
    let url = (|| -> Option<String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;

        rt.block_on(async move {
            let mut cmd = tokio::process::Command::new("git");
            cmd.args(["remote", "get-url", "origin"]);
            cmd.current_dir(&root_dir_buf);
            cmd.stdin(std::process::Stdio::null());
            cmd.kill_on_drop(true);

            let child = cmd
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .ok()?;

            let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

            match result {
                Ok(Ok(output)) if output.status.success() => {
                    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                }
                _ => None,
            }
        })
    })();

    let input = url.unwrap_or_else(|| root_dir.to_string_lossy().to_string());

    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Store a tailoring result in the config cache (best-effort).
///
/// Skips caching when `cache_key` is `None`.
/// Rejects outputs larger than `CACHE_MAX_SIZE_BYTES` (10 MiB) when serialized.
/// Disk persistence failures are silently ignored because the cache
/// is an optimisation — a write failure should never abort a sync.
pub(crate) fn store_tailoring_cache(
    config: &mut crate::config::types::Config,
    cfg_path: &Path,
    cache_key: Option<&str>,
    root_dir: &Path,
    output: &TailoringOutput,
) {
    let Some(key) = cache_key else {
        return;
    };

    // Size guard: skip caching if the serialized output exceeds the limit.
    let serialized_size = serde_yml::to_string(output).map(|s| s.len()).unwrap_or(0);
    if serialized_size > CACHE_MAX_SIZE_BYTES {
        tracing::warn!(
            "tailoring output size ({} bytes) exceeds cache limit ({} bytes); skipping cache",
            serialized_size,
            CACHE_MAX_SIZE_BYTES
        );
        return;
    }

    config.cached_tailoring = Some(CachedTailoring {
        cache_key: key.to_string(),
        repo_path: root_dir.to_string_lossy().to_string(),
        tailoring: output.clone(),
        tailored_at: chrono::Utc::now(),
    });
    // Best-effort persist: if disk write fails, the in-memory cache is
    // still populated for subsequent operations in this process.
    if let Err(e) = save_to(config, cfg_path) {
        tracing::warn!("failed to save tailoring cache: {e}");
    }
}

/// Compute a cache key for the tailoring step by hashing all inputs
/// that affect the tailoring output.
///
/// The key incorporates: full ADR content (sorted by ID for determinism),
/// the no_tailor flag, sorted project paths, existing CLAUDE.md content,
/// and model override. Any change in these inputs produces a different cache
/// key. The git HEAD commit is intentionally excluded so that commits
/// unrelated to ADR content or repo structure do not invalidate the cache.
pub(crate) fn compute_tailoring_cache_key(
    adrs: &[crate::api::types::Adr],
    no_tailor: bool,
    project_paths: &[String],
    existing_claude_md: &str,
    model: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();

    // Sort ADRs by ID for determinism regardless of iteration order.
    let mut sorted_adrs: Vec<&crate::api::types::Adr> = adrs.iter().collect();
    sorted_adrs.sort_by(|a, b| a.id.cmp(&b.id));

    // Field-boundary bytes used below:
    //   \x00 — value separator within a list element (prevents "ab"+"c" == "a"+"bc")
    //   \xfe — list-field separator within an ADR (prevents policies/instructions
    //           ambiguity: ["a"]+[] == []+["a"] without a separator)
    //   \xff — ADR separator between successive ADRs in the outer loop (prevents
    //           ["a","b"]+["c"] == ["a"]+["b","c"] across ADR boundaries)
    for adr in sorted_adrs {
        hasher.update(adr.id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.title.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.context.as_deref().unwrap_or("").as_bytes());
        hasher.update(b"\x00");

        let mut policies = adr.policies.clone();
        policies.sort();
        for policy in &policies {
            hasher.update(policy.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of policies list

        let mut instructions = adr.instructions.clone().unwrap_or_default();
        instructions.sort();
        for instruction in &instructions {
            hasher.update(instruction.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of instructions list

        // AdrCategory: hash its id, name, and path fields.
        hasher.update(adr.category.id.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.category.name.as_bytes());
        hasher.update(b"\x00");
        hasher.update(adr.category.path.as_bytes());
        hasher.update(b"\x00");

        // AppliesTo: hash sorted languages and frameworks.
        let mut languages = adr.applies_to.languages.clone();
        languages.sort();
        for lang in &languages {
            hasher.update(lang.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of languages list

        let mut frameworks = adr.applies_to.frameworks.clone();
        frameworks.sort();
        for fw in &frameworks {
            hasher.update(fw.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of frameworks list

        // matched_projects (sorted).
        let mut matched = adr.matched_projects.clone();
        matched.sort();
        for mp in &matched {
            hasher.update(mp.as_bytes());
            hasher.update(b"\x00");
        }
        hasher.update(b"\xfe"); // end of matched_projects list
                                // ADR boundary marker: prevents two distinct ADR sets from producing
                                // the same byte stream (e.g. ["a", "b"] + ["c"] vs ["a"] + ["b", "c"]).
        hasher.update(b"\xff");
    }

    hasher.update(if no_tailor {
        &b"no_tailor"[..]
    } else {
        &b"tailor"[..]
    });
    hasher.update(b"\x00");
    for path in project_paths {
        hasher.update(path.as_bytes());
        hasher.update(b"\x00");
    }
    hasher.update(existing_claude_md.as_bytes());
    hasher.update(b"\x00");
    if let Some(m) = model {
        hasher.update(m.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub(crate) fn load_config_with_fallback(cfg_path: &std::path::Path) -> crate::config::Config {
    match load_from(cfg_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "failed to load config from {}: {e} — using defaults",
                cfg_path.display()
            );
            Default::default()
        }
    }
}
