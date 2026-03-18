use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::process::Command;

use super::{EvidenceSpan, SignalSource, ToolMatch};

/// Extract embedded semgrep rules to a temporary directory.
/// Returns the temp dir (caller must keep it alive) and the list of rule file paths.
pub fn extract_embedded_rules() -> Result<(tempfile::TempDir, Vec<std::path::PathBuf>)> {
    use super::embedded::EmbeddedSemgrepRules;

    let tmp = tempfile::TempDir::new().context("failed to create temp dir for semgrep rules")?;
    let mut rule_paths = Vec::new();

    for filename in EmbeddedSemgrepRules::iter() {
        let file = EmbeddedSemgrepRules::get(&filename)
            .expect("embedded file listed by iter() must be gettable");
        let dest = tmp.path().join(filename.as_ref());
        std::fs::create_dir_all(dest.parent().unwrap_or(tmp.path()))?;
        std::fs::write(&dest, file.data.as_ref())?;
        rule_paths.push(dest);
    }

    Ok((tmp, rule_paths))
}

/// Maximum size of a single file written to the semgrep temp directory (10 MB).
const MAX_FILE_SIZE_BYTES: usize = 10 * 1024 * 1024;

/// Maximum number of files in a single semgrep batch.
const MAX_BATCH_FILES: usize = 1000;

/// Maximum total content size across all files in a single semgrep batch (100 MB).
const MAX_BATCH_SIZE_BYTES: usize = 100 * 1024 * 1024;

/// Canonicalize `path`, returning an error that includes the path in the
/// message rather than silently swallowing the failure.
///
/// Extracted as a standalone function so the error path is directly testable.
fn canonicalize_with_context(path: &std::path::Path) -> Result<PathBuf> {
    std::fs::canonicalize(path)
        .with_context(|| format!("failed to canonicalize temp path {path:?}"))
}

/// Output from running the semgrep command.
#[derive(Debug)]
struct CommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// The raw exit code from the process, or `None` if the process was
    /// terminated by a signal (Unix) / could not be queried.
    exit_code: Option<i32>,
}

/// Async scanner that invokes the semgrep CLI on batches of files.
pub struct SemgrepScanner {
    semgrep_bin: PathBuf,
    timeout: std::time::Duration,
}

impl SemgrepScanner {
    /// Create a new scanner, verifying that the semgrep binary is available.
    pub fn new(timeout: std::time::Duration) -> Result<Self> {
        Self::with_name("semgrep", timeout)
    }

    /// Create a scanner by looking up a named binary in PATH.
    fn with_name(name: &str, timeout: std::time::Duration) -> Result<Self> {
        let semgrep_bin =
            which::which(name).with_context(|| format!("{name} binary not found in PATH"))?;
        Ok(Self {
            semgrep_bin,
            timeout,
        })
    }

    /// Create a scanner with an explicit binary path (for tests).
    #[cfg(test)]
    fn with_binary(semgrep_bin: PathBuf, timeout: std::time::Duration) -> Self {
        Self {
            semgrep_bin,
            timeout,
        }
    }

    /// Create a scanner with an explicit binary path — accessible from all
    /// test modules in the crate (e.g. `pipeline` tests that need a scanner
    /// but cannot reach the private `with_binary` method).
    #[cfg(test)]
    pub(crate) fn with_binary_for_pipeline_test(
        semgrep_bin: PathBuf,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            semgrep_bin,
            timeout,
        }
    }

    /// Prepare temp directory with files and return (temp_dir, file_map).
    fn prepare_temp_files(
        files: &[(PathBuf, String)],
    ) -> Result<(tempfile::TempDir, HashMap<PathBuf, PathBuf>)> {
        let temp_dir = tempfile::tempdir().context("failed to create temp directory")?;
        let mut file_map: HashMap<PathBuf, PathBuf> = HashMap::new();

        for (i, (rel_path, content)) in files.iter().enumerate() {
            if content.len() > MAX_FILE_SIZE_BYTES {
                let sz = content.len();
                bail!("file too large for analysis: {sz} bytes (max {MAX_FILE_SIZE_BYTES}): {rel_path:?}");
            }

            let subdir = temp_dir.path().join(format!("f{i}"));
            std::fs::create_dir_all(&subdir).context("failed to create temp subdir")?;

            let file_name = rel_path
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("file.txt"));
            let temp_path = subdir.join(file_name);

            std::fs::write(&temp_path, content).context("failed to write temp file")?;

            // Canonicalize so the key matches what semgrep emits (semgrep
            // resolves symlinks, e.g. /tmp → /private/tmp on macOS).
            // We propagate errors here rather than silently falling back to
            // the raw path: the file was just written, so canonicalization
            // should never fail in practice, and if it does we want to know.
            let canonical = canonicalize_with_context(&temp_path)?;
            file_map.insert(canonical, rel_path.clone());
        }

        Ok((temp_dir, file_map))
    }

    /// Build the argument list for semgrep.
    ///
    /// Uses `OsString` to preserve non-UTF-8 paths without lossy conversion.
    fn build_args(rule_files: &[PathBuf], scan_dir: &std::path::Path) -> Vec<std::ffi::OsString> {
        let mut args: Vec<std::ffi::OsString> = vec![
            "scan".into(),
            "--json".into(),
            "--no-git-ignore".into(),
            "--metrics=off".into(),
        ];
        for rule_file in rule_files {
            args.push("--config".into());
            args.push(rule_file.into());
        }
        args.push(scan_dir.into());
        args
    }

    /// Returns `true` when the semgrep output indicates an authentication /
    /// login requirement.
    ///
    /// Semgrep exit code 7 is the canonical signal for "authentication
    /// required".  When that is not available we fall back to specific
    /// phrases that appear in real semgrep auth-error messages.  A bare
    /// "login" substring is intentionally **not** matched because it fires
    /// on legitimate stderr output such as "scanning login.py".
    fn is_auth_error(exit_code: Option<i32>, stderr: &str) -> bool {
        if exit_code == Some(7) {
            return true;
        }
        let lower = stderr.to_ascii_lowercase();
        lower.contains("semgrep login") || lower.contains("authentication required")
    }

    /// Process raw command output into tool matches.
    fn process_output(
        output: &CommandOutput,
        file_map: &HashMap<PathBuf, PathBuf>,
    ) -> Result<Vec<ToolMatch>> {
        let stdout = String::from_utf8_lossy(&output.stdout);

        if stdout.trim().is_empty() {
            if output.exit_code == Some(0) {
                return Ok(vec![]);
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            if Self::is_auth_error(output.exit_code, &stderr) {
                bail!(
                    "semgrep requires login (v1.100+). \
                     Try running `semgrep login` or use an older version. \
                     stderr: {stderr}"
                );
            }
            bail!("semgrep failed with no JSON output. stderr: {stderr}");
        }

        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).context("failed to parse semgrep JSON output")?;

        Self::parse_findings(&parsed, file_map)
    }

    /// Scan a batch of files against the given semgrep rule files.
    ///
    /// Each entry in `files` is `(relative_path, file_content)`.
    /// Returns all matches found across all files.
    pub async fn scan_batch(
        &self,
        files: &[(PathBuf, String)],
        rule_files: &[PathBuf],
    ) -> Result<Vec<ToolMatch>> {
        if files.is_empty() || rule_files.is_empty() {
            return Ok(vec![]);
        }

        if files.len() > MAX_BATCH_FILES {
            let n = files.len();
            bail!("batch too large: {n} files (max {MAX_BATCH_FILES})");
        }

        let batch_size: usize = files.iter().map(|(_, c)| c.len()).sum();
        if batch_size > MAX_BATCH_SIZE_BYTES {
            bail!("batch too large: {batch_size} bytes total (max {MAX_BATCH_SIZE_BYTES})");
        }

        let (temp_dir, file_map) = Self::prepare_temp_files(files)?;
        let args = Self::build_args(rule_files, temp_dir.path());

        // Build and run the command.  `kill_on_drop(true)` ensures the
        // child is killed if the future is cancelled by a timeout rather
        // than leaving an orphan process.
        let mut cmd = Command::new(&self.semgrep_bin);
        cmd.kill_on_drop(true);
        for arg in &args {
            cmd.arg(arg);
        }

        let raw_output = tokio::time::timeout(self.timeout, cmd.output())
            .await
            .context("semgrep scan timed out")?
            .context("failed to execute semgrep")?;

        let output = CommandOutput {
            stdout: raw_output.stdout,
            stderr: raw_output.stderr,
            exit_code: raw_output.status.code(),
        };

        Self::process_output(&output, &file_map)
    }

    /// Parse semgrep JSON output into a list of `ToolMatch` values.
    ///
    /// `file_map` maps temp file paths back to original relative paths.
    fn parse_findings(
        output: &serde_json::Value,
        file_map: &HashMap<PathBuf, PathBuf>,
    ) -> Result<Vec<ToolMatch>> {
        let results = match output.get("results") {
            Some(serde_json::Value::Array(arr)) => arr,
            Some(_) => bail!("semgrep 'results' field is not an array"),
            None => return Ok(vec![]),
        };

        let mut matches = Vec::with_capacity(results.len());

        for finding in results {
            let check_id = finding
                .get("check_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let path_str = finding.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let temp_path = PathBuf::from(path_str);

            // Map temp path back to original path.  Canonicalize for lookup
            // since file_map keys were canonicalized at insertion time.
            let lookup_key =
                std::fs::canonicalize(&temp_path).unwrap_or_else(|_| temp_path.clone());
            let original_path = file_map
                .get(&lookup_key)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| path_str.to_string());

            // Extract byte offsets and line numbers
            let start = finding.get("start");
            let end = finding.get("end");

            let start_offset = start
                .and_then(|s| s.get("offset"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let end_offset = end
                .and_then(|s| s.get("offset"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let start_line = start
                .and_then(|s| s.get("line"))
                .and_then(|v| v.as_u64())
                .map(|l| l as usize);
            let end_line = end
                .and_then(|s| s.get("line"))
                .and_then(|v| v.as_u64())
                .map(|l| l as usize);

            // Extract metadata
            let extra = finding.get("extra");
            let metadata = extra.and_then(|e| e.get("metadata"));

            let facet_slot = metadata
                .and_then(|m| m.get("facet_slot"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let leaf_id = metadata
                .and_then(|m| m.get("leaf_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let confidence: f64 = metadata
                .and_then(|m| m.get("confidence"))
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| v.as_f64())
                })
                .unwrap_or(0.0);

            let matched_lines = extra
                .and_then(|e| e.get("lines"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            let span = EvidenceSpan {
                file_path: original_path,
                start_byte: start_offset,
                end_byte: end_offset,
                start_line,
                end_line,
                source: SignalSource::Semgrep,
            };

            let tool_match = ToolMatch {
                rule_id: check_id,
                facet_slot,
                leaf_id,
                value: matched_lines,
                confidence,
                spans: vec![span],
                raw: Some(finding.clone()),
            };

            matches.push(tool_match);
        }

        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_semgrep_output() -> serde_json::Value {
        serde_json::json!({
            "results": [
                {
                    "check_id": "js.express.route",
                    "path": "/tmp/scan/f0/app.js",
                    "start": {"line": 5, "col": 1, "offset": 42},
                    "end": {"line": 5, "col": 30, "offset": 71},
                    "extra": {
                        "metadata": {
                            "facet_slot": "boundaries.service_definitions",
                            "leaf_id": "23",
                            "confidence": "0.9"
                        },
                        "lines": "app.get('/api/users', handler)"
                    }
                },
                {
                    "check_id": "py.flask.route",
                    "path": "/tmp/scan/f1/server.py",
                    "start": {"line": 10, "col": 1, "offset": 120},
                    "end": {"line": 12, "col": 5, "offset": 200},
                    "extra": {
                        "metadata": {
                            "facet_slot": "boundaries.service_definitions",
                            "leaf_id": "24",
                            "confidence": "0.85"
                        },
                        "lines": "@app.route('/health')\ndef health():\n    return 'ok'"
                    }
                }
            ],
            "errors": [],
            "version": "1.50.0"
        })
    }

    // ---- parse_findings tests ----

    #[test]
    fn test_parse_findings_basic() {
        let output = sample_semgrep_output();
        let mut file_map = HashMap::new();
        file_map.insert(
            PathBuf::from("/tmp/scan/f0/app.js"),
            PathBuf::from("src/app.js"),
        );
        file_map.insert(
            PathBuf::from("/tmp/scan/f1/server.py"),
            PathBuf::from("src/server.py"),
        );

        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        assert_eq!(matches.len(), 2);

        let m0 = &matches[0];
        assert_eq!(m0.rule_id, "js.express.route");
        assert_eq!(m0.facet_slot, "boundaries.service_definitions");
        assert_eq!(m0.leaf_id, "23");
        assert!((m0.confidence - 0.9).abs() < f64::EPSILON);
        assert_eq!(m0.spans.len(), 1);
        assert_eq!(m0.spans[0].file_path, "src/app.js");
        assert_eq!(m0.spans[0].start_byte, 42);
        assert_eq!(m0.spans[0].end_byte, 71);
        assert_eq!(m0.spans[0].start_line, Some(5));
        assert_eq!(m0.spans[0].end_line, Some(5));
        assert_eq!(m0.spans[0].source, SignalSource::Semgrep);

        let m1 = &matches[1];
        assert_eq!(m1.rule_id, "py.flask.route");
        assert_eq!(m1.leaf_id, "24");
        assert!((m1.confidence - 0.85).abs() < f64::EPSILON);
        assert_eq!(m1.spans[0].file_path, "src/server.py");
        assert_eq!(m1.spans[0].start_line, Some(10));
        assert_eq!(m1.spans[0].end_line, Some(12));
    }

    #[test]
    fn test_parse_findings_empty_results() {
        let output = serde_json::json!({"results": [], "errors": []});
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_parse_findings_no_results_key() {
        let output = serde_json::json!({"errors": []});
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_parse_findings_results_not_array() {
        let output = serde_json::json!({"results": "not_an_array"});
        let file_map = HashMap::new();
        let result = SemgrepScanner::parse_findings(&output, &file_map);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not an array"));
    }

    #[test]
    fn test_parse_findings_missing_metadata() {
        let output = serde_json::json!({
            "results": [
                {
                    "check_id": "generic.rule",
                    "path": "/tmp/scan/f0/file.txt",
                    "start": {"line": 1, "col": 1, "offset": 0},
                    "end": {"line": 1, "col": 10, "offset": 9},
                    "extra": {}
                }
            ]
        });
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].facet_slot, "");
        assert_eq!(matches[0].leaf_id, "");
        assert!((matches[0].confidence - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_findings_numeric_confidence() {
        let output = serde_json::json!({
            "results": [
                {
                    "check_id": "test.rule",
                    "path": "/tmp/f0/test.js",
                    "start": {"line": 1, "col": 1, "offset": 0},
                    "end": {"line": 1, "col": 5, "offset": 4},
                    "extra": {
                        "metadata": {
                            "facet_slot": "slot",
                            "leaf_id": "1",
                            "confidence": 0.75
                        }
                    }
                }
            ]
        });
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        assert!((matches[0].confidence - 0.75).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_findings_unmapped_path() {
        let output = serde_json::json!({
            "results": [
                {
                    "check_id": "rule",
                    "path": "/some/unknown/path.js",
                    "start": {"line": 1, "col": 1, "offset": 0},
                    "end": {"line": 1, "col": 5, "offset": 4},
                    "extra": {
                        "metadata": {
                            "facet_slot": "s",
                            "leaf_id": "1",
                            "confidence": "0.5"
                        }
                    }
                }
            ]
        });
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        assert_eq!(matches[0].spans[0].file_path, "/some/unknown/path.js");
    }

    #[test]
    fn test_parse_findings_raw_preserved() {
        let output = sample_semgrep_output();
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        for m in &matches {
            assert!(m.raw.is_some());
            let raw = m.raw.as_ref().unwrap();
            assert!(raw.get("check_id").is_some());
        }
    }

    #[test]
    fn test_parse_findings_no_extra() {
        let output = serde_json::json!({
            "results": [
                {
                    "check_id": "rule",
                    "path": "/tmp/f0/test.js",
                    "start": {"line": 1, "col": 1, "offset": 0},
                    "end": {"line": 1, "col": 5, "offset": 4}
                }
            ]
        });
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].facet_slot, "");
        assert_eq!(matches[0].leaf_id, "");
        assert!(matches[0].value.is_null());
    }

    #[test]
    fn test_parse_findings_no_start_end() {
        let output = serde_json::json!({
            "results": [
                {
                    "check_id": "rule",
                    "path": "/tmp/f0/test.js"
                }
            ]
        });
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].spans[0].start_byte, 0);
        assert_eq!(matches[0].spans[0].end_byte, 0);
        assert_eq!(matches[0].spans[0].start_line, None);
        assert_eq!(matches[0].spans[0].end_line, None);
    }

    #[test]
    fn test_parse_findings_missing_check_id() {
        let output = serde_json::json!({
            "results": [
                {
                    "path": "/tmp/f0/test.js",
                    "start": {"line": 1, "col": 1, "offset": 0},
                    "end": {"line": 1, "col": 5, "offset": 4}
                }
            ]
        });
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        assert_eq!(matches[0].rule_id, "");
    }

    // ---- prepare_temp_files tests ----

    #[test]
    fn test_prepare_temp_files_creates_structure() {
        let files = vec![
            (PathBuf::from("src/app.js"), "const x = 1;".to_string()),
            (PathBuf::from("src/server.py"), "print('hi')".to_string()),
        ];
        let (temp_dir, file_map) = SemgrepScanner::prepare_temp_files(&files).unwrap();
        assert_eq!(file_map.len(), 2);

        for (temp_path, original_path) in &file_map {
            assert!(temp_path.exists(), "temp file should exist: {temp_path:?}");
            let expected_name = original_path.file_name().unwrap();
            assert_eq!(temp_path.file_name().unwrap(), expected_name);
        }

        assert!(temp_dir.path().join("f0").is_dir());
        assert!(temp_dir.path().join("f1").is_dir());
    }

    #[test]
    fn test_prepare_temp_files_path_without_filename() {
        let files = vec![(PathBuf::from(""), "content".to_string())];
        let (_temp_dir, file_map) = SemgrepScanner::prepare_temp_files(&files).unwrap();
        assert_eq!(file_map.len(), 1);
        for (temp_path, _) in &file_map {
            assert_eq!(temp_path.file_name().unwrap(), "file.txt");
        }
    }

    #[test]
    fn test_prepare_temp_files_empty() {
        let files: Vec<(PathBuf, String)> = vec![];
        let (_temp_dir, file_map) = SemgrepScanner::prepare_temp_files(&files).unwrap();
        assert!(file_map.is_empty());
    }

    #[test]
    fn test_prepare_temp_files_content_preserved() {
        let content = "function hello() { return 42; }";
        let files = vec![(PathBuf::from("test.js"), content.to_string())];
        let (_temp_dir, file_map) = SemgrepScanner::prepare_temp_files(&files).unwrap();
        for (temp_path, _) in &file_map {
            let read_back = std::fs::read_to_string(temp_path).unwrap();
            assert_eq!(read_back, content);
        }
    }

    // ---- build_args tests ----

    #[test]
    fn test_build_args() {
        let rule_files = vec![
            PathBuf::from("/rules/api.yml"),
            PathBuf::from("/rules/security.yml"),
        ];
        let scan_dir = std::path::Path::new("/tmp/scan");
        let args = SemgrepScanner::build_args(&rule_files, scan_dir);
        assert_eq!(args[0], "scan");
        assert_eq!(args[1], "--json");
        assert_eq!(args[2], "--no-git-ignore");
        assert_eq!(args[3], "--metrics=off");
        assert_eq!(args[4], "--config");
        assert_eq!(args[5], "/rules/api.yml");
        assert_eq!(args[6], "--config");
        assert_eq!(args[7], "/rules/security.yml");
        assert_eq!(args[8], "/tmp/scan");
    }

    #[test]
    fn test_build_args_empty_rules() {
        let scan_dir = std::path::Path::new("/tmp/scan");
        let args = SemgrepScanner::build_args(&[], scan_dir);
        assert_eq!(args.len(), 5);
        assert_eq!(args[4], "/tmp/scan");
    }

    // ---- process_output tests ----

    #[test]
    fn test_process_output_with_valid_json() {
        let json = serde_json::json!({
            "results": [
                {
                    "check_id": "rule1",
                    "path": "/tmp/f0/test.js",
                    "start": {"line": 1, "col": 1, "offset": 0},
                    "end": {"line": 1, "col": 5, "offset": 4},
                    "extra": {
                        "metadata": {"facet_slot": "s", "leaf_id": "1", "confidence": "0.5"}
                    }
                }
            ]
        });
        let output = CommandOutput {
            stdout: json.to_string().into_bytes(),
            stderr: Vec::new(),
            exit_code: Some(0),
        };
        let mut file_map = HashMap::new();
        file_map.insert(
            PathBuf::from("/tmp/f0/test.js"),
            PathBuf::from("src/test.js"),
        );
        let result = SemgrepScanner::process_output(&output, &file_map).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].spans[0].file_path, "src/test.js");
    }

    #[test]
    fn test_process_output_empty_success() {
        let output = CommandOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: Some(0),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_process_output_whitespace_only_success() {
        let output = CommandOutput {
            stdout: b"   \n  ".to_vec(),
            stderr: Vec::new(),
            exit_code: Some(0),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_process_output_empty_failure() {
        let output = CommandOutput {
            stdout: Vec::new(),
            stderr: b"error occurred".to_vec(),
            exit_code: Some(1),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no JSON output"));
    }

    // ---- is_auth_error / process_output auth detection tests ----

    /// Exit code 7 is the canonical semgrep auth error — always treated as auth.
    #[test]
    fn test_process_output_auth_error_exit_code_7() {
        let output = CommandOutput {
            stdout: Vec::new(),
            stderr: b"Please run `semgrep login` to authenticate".to_vec(),
            exit_code: Some(7),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("semgrep requires login"),
            "expected auth error message, got: {err}"
        );
    }

    /// Exit code 7 triggers auth error even with an empty stderr.
    #[test]
    fn test_process_output_auth_error_exit_code_7_empty_stderr() {
        let output = CommandOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: Some(7),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("semgrep requires login"),
            "expected auth error message, got: {err}"
        );
    }

    /// "semgrep login" phrase in stderr triggers auth error (non-7 exit).
    #[test]
    fn test_process_output_auth_error_semgrep_login_phrase() {
        let output = CommandOutput {
            stdout: Vec::new(),
            stderr: b"Please run semgrep login to continue".to_vec(),
            exit_code: Some(1),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("semgrep requires login"),
            "expected auth error message, got: {err}"
        );
    }

    /// "authentication required" phrase in stderr triggers auth error.
    #[test]
    fn test_process_output_auth_error_authentication_required_phrase() {
        let output = CommandOutput {
            stdout: Vec::new(),
            stderr: b"authentication required to use this feature".to_vec(),
            exit_code: Some(1),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("semgrep requires login"),
            "expected auth error message, got: {err}"
        );
    }

    /// Bare "login" in stderr (e.g. scanning login.py) must NOT trigger auth
    /// error — it should fall through to the generic error.
    #[test]
    fn test_process_output_false_positive_login_word_in_stderr() {
        let output = CommandOutput {
            stdout: Vec::new(),
            stderr: b"scanning login.py for vulnerabilities".to_vec(),
            exit_code: Some(1),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no JSON output"),
            "expected generic error, got: {err}"
        );
        assert!(
            !err.contains("semgrep requires login"),
            "should NOT produce auth error for bare 'login' word: {err}"
        );
    }

    /// "Login required" (no "semgrep" prefix) is not specific enough — generic error.
    #[test]
    fn test_process_output_generic_login_required_is_not_auth_error() {
        let output = CommandOutput {
            stdout: Vec::new(),
            stderr: b"Login required for this operation".to_vec(),
            exit_code: Some(1),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no JSON output"),
            "expected generic error, got: {err}"
        );
    }

    /// Exit 0 with empty stdout is a clean success — no error regardless of stderr.
    #[test]
    fn test_process_output_exit_0_is_success_regardless_of_stderr() {
        let output = CommandOutput {
            stdout: Vec::new(),
            stderr: b"semgrep login".to_vec(),
            exit_code: Some(0),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_process_output_invalid_json() {
        let output = CommandOutput {
            stdout: b"not json".to_vec(),
            stderr: Vec::new(),
            exit_code: Some(0),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to parse"));
    }

    #[test]
    fn test_process_output_non_zero_with_valid_json() {
        // semgrep can exit non-zero but still produce valid JSON
        let json = serde_json::json!({"results": [], "errors": [{"message": "some warning"}]});
        let output = CommandOutput {
            stdout: json.to_string().into_bytes(),
            stderr: b"warning: something".to_vec(),
            exit_code: Some(2),
        };
        let file_map = HashMap::new();
        let result = SemgrepScanner::process_output(&output, &file_map).unwrap();
        assert!(result.is_empty());
    }

    // ---- is_auth_error unit tests ----

    #[test]
    fn test_is_auth_error_exit_code_7_no_stderr() {
        assert!(SemgrepScanner::is_auth_error(Some(7), ""));
    }

    #[test]
    fn test_is_auth_error_exit_code_7_any_stderr() {
        assert!(SemgrepScanner::is_auth_error(Some(7), "scanning login.py"));
    }

    #[test]
    fn test_is_auth_error_semgrep_login_phrase_lowercase() {
        assert!(SemgrepScanner::is_auth_error(
            Some(1),
            "please run semgrep login"
        ));
    }

    #[test]
    fn test_is_auth_error_semgrep_login_phrase_mixed_case() {
        assert!(SemgrepScanner::is_auth_error(
            Some(1),
            "Run `Semgrep Login` to authenticate"
        ));
    }

    #[test]
    fn test_is_auth_error_authentication_required() {
        assert!(SemgrepScanner::is_auth_error(
            Some(1),
            "Authentication Required to use this rule"
        ));
    }

    #[test]
    fn test_is_auth_error_bare_login_word_is_not_auth() {
        assert!(!SemgrepScanner::is_auth_error(Some(1), "scanning login.py"));
    }

    #[test]
    fn test_is_auth_error_unrelated_stderr_is_not_auth() {
        assert!(!SemgrepScanner::is_auth_error(Some(1), "parse error"));
    }

    #[test]
    fn test_is_auth_error_none_exit_code_no_phrase() {
        assert!(!SemgrepScanner::is_auth_error(None, "some error"));
    }

    #[test]
    fn test_is_auth_error_none_exit_code_with_phrase() {
        assert!(SemgrepScanner::is_auth_error(
            None,
            "please run semgrep login"
        ));
    }

    // ---- extract_embedded_rules tests ----

    #[test]
    fn extract_embedded_rules_creates_files() {
        let (tmp_dir, rule_paths) =
            super::extract_embedded_rules().expect("extract_embedded_rules should succeed");
        assert!(!rule_paths.is_empty(), "expected at least one rule file");
        // Verify files actually exist on disk
        for path in &rule_paths {
            assert!(path.exists());
        }
        // tmp_dir cleanup happens on drop
        drop(tmp_dir);
    }

    // ---- scanner creation tests ----

    #[test]
    fn test_new_delegates_to_with_name() {
        // new() is a one-liner: Self::with_name("semgrep", timeout).
        // Verify that new() and with_name("semgrep", ...) agree on
        // success/failure. This is not a pure tautology: if new() were
        // accidentally changed to look up a different binary name, the two
        // calls would disagree when exactly one of those names is in PATH.
        // The delegation body is also auditable by inspection (one line).
        let timeout = std::time::Duration::from_secs(30);
        let result_new = SemgrepScanner::new(timeout);
        let result_with_name = SemgrepScanner::with_name("semgrep", timeout);
        assert_eq!(result_new.is_ok(), result_with_name.is_ok());
    }

    #[test]
    fn test_with_name_finds_existing_binary() {
        // Use "sh" which exists on all Unix systems, exercising the Ok path
        // of with_name (and transitively new's internal logic).
        let result = SemgrepScanner::with_name("sh", std::time::Duration::from_secs(5));
        assert!(result.is_ok());
        let scanner = result.unwrap();
        assert!(!scanner.semgrep_bin.as_os_str().is_empty());
        assert_eq!(scanner.timeout, std::time::Duration::from_secs(5));
    }

    #[test]
    fn test_with_name_fails_for_nonexistent_binary() {
        let result = SemgrepScanner::with_name(
            "nonexistent_binary_xyz_12345",
            std::time::Duration::from_secs(5),
        );
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("not found in PATH"));
    }

    // ---- scan_batch early-return tests ----

    #[tokio::test]
    async fn test_scan_batch_empty_files() {
        // scan_batch returns early when files is empty, never runs the binary
        let scanner = SemgrepScanner::with_binary(
            PathBuf::from("/nonexistent/semgrep"),
            std::time::Duration::from_secs(5),
        );
        let result = scanner.scan_batch(&[], &[PathBuf::from("rule.yml")]).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_scan_batch_empty_rules() {
        let scanner = SemgrepScanner::with_binary(
            PathBuf::from("/nonexistent/semgrep"),
            std::time::Duration::from_secs(5),
        );
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner.scan_batch(&files, &[]).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_scan_batch_both_empty() {
        let scanner = SemgrepScanner::with_binary(
            PathBuf::from("/nonexistent/semgrep"),
            std::time::Duration::from_secs(5),
        );
        let result = scanner.scan_batch(&[], &[]).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_scan_batch_nonexistent_binary() {
        let scanner = SemgrepScanner::with_binary(
            PathBuf::from("/nonexistent/binary/semgrep"),
            std::time::Duration::from_secs(5),
        );
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("failed to execute semgrep"));
    }

    #[tokio::test]
    async fn test_scan_batch_with_echo_script() {
        // Create a fake "semgrep" script that outputs valid JSON
        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("fake_semgrep.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\necho '{\"results\": [], \"errors\": []}'",
        )
        .unwrap();

        // Make executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let scanner = SemgrepScanner::with_binary(script_path, std::time::Duration::from_secs(5));
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_scan_batch_script_with_findings() {
        // This script discovers the actual temp dir from its last argument (the scan dir)
        // and outputs JSON with a path that matches what prepare_temp_files creates,
        // so that the path-remapping logic in process_output is exercised.
        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("fake_semgrep_findings.sh");
        // The script uses $SCAN_DIR (last argument) to construct a path matching
        // the temp file layout: <scan_dir>/f0/test.js
        let script_content = r#"#!/bin/sh
# Last argument is the scan directory
for arg; do SCAN_DIR="$arg"; done
cat <<EOJSON
{"results": [{"check_id": "test.rule", "path": "${SCAN_DIR}/f0/test.js", "start": {"line": 1, "col": 1, "offset": 0}, "end": {"line": 1, "col": 10, "offset": 9}, "extra": {"metadata": {"facet_slot": "boundaries.api", "leaf_id": "99", "confidence": "0.8"}, "lines": "const x = 1;"}}]}
EOJSON
"#;
        std::fs::write(&script_path, script_content).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let scanner = SemgrepScanner::with_binary(script_path, std::time::Duration::from_secs(5));
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_ok());
        let matches = result.unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].rule_id, "test.rule");
        assert_eq!(matches[0].facet_slot, "boundaries.api");
        // Verify path remapping: the temp path should be mapped back to the original
        assert_eq!(
            matches[0].spans[0].file_path, "test.js",
            "path should be remapped from temp dir to original relative path"
        );
    }

    /// Semgrep exits with code 7 (canonical auth error) → auth-specific message.
    #[tokio::test]
    async fn test_scan_batch_script_auth_error_exit_code_7() {
        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("fake_semgrep_auth7.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\necho 'Please run `semgrep login`' >&2\nexit 7",
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let scanner = SemgrepScanner::with_binary(script_path, std::time::Duration::from_secs(5));
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("semgrep requires login"),
            "expected auth error, got: {err}"
        );
    }

    /// stderr mentioning "scanning login.py" with non-auth exit → generic error,
    /// not the misleading auth error.
    #[tokio::test]
    async fn test_scan_batch_script_login_word_in_stderr_is_not_auth_error() {
        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("fake_semgrep_login_file.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\necho 'scanning login.py for vulnerabilities' >&2\nexit 1",
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let scanner = SemgrepScanner::with_binary(script_path, std::time::Duration::from_secs(5));
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no JSON output"),
            "expected generic error, got: {err}"
        );
        assert!(
            !err.contains("semgrep requires login"),
            "should not produce auth error for bare 'login' word: {err}"
        );
    }

    #[tokio::test]
    async fn test_scan_batch_script_empty_failure() {
        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("fake_semgrep_fail.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\necho 'something went wrong' >&2\nexit 2",
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let scanner = SemgrepScanner::with_binary(script_path, std::time::Duration::from_secs(5));
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no JSON output"));
    }

    #[tokio::test]
    async fn test_scan_batch_script_invalid_json() {
        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("fake_semgrep_badjson.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho 'not valid json'").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let scanner = SemgrepScanner::with_binary(script_path, std::time::Duration::from_secs(5));
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed to parse"));
    }

    // ---- file size / batch size limit tests (g5j.16, g5j.19) ----

    #[test]
    fn test_canonicalize_with_context_ok() {
        // Canonicalize a path that exists (the temp dir itself).
        let dir = tempfile::tempdir().unwrap();
        let result = canonicalize_with_context(dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_canonicalize_with_context_error() {
        // Canonicalize a path that does not exist — should return Err with a
        // message containing the path.
        let dir = tempfile::tempdir().unwrap();
        let nonexistent = dir.path().join("does_not_exist");
        let result = canonicalize_with_context(&nonexistent);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("failed to canonicalize"),
            "expected 'failed to canonicalize' in error, got: {msg}"
        );
    }

    #[test]
    fn test_prepare_temp_files_rejects_oversized_file() {
        // A single file whose content exceeds MAX_FILE_SIZE_BYTES should cause
        // prepare_temp_files to return an error.
        let big_content = "x".repeat(MAX_FILE_SIZE_BYTES + 1);
        let files = vec![(PathBuf::from("big.js"), big_content)];
        let result = SemgrepScanner::prepare_temp_files(&files);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("too large"),
            "expected 'too large' in error, got: {msg}"
        );
    }

    #[test]
    fn test_prepare_temp_files_accepts_file_at_exact_limit() {
        // A file exactly at the limit should be accepted.
        let content = "x".repeat(MAX_FILE_SIZE_BYTES);
        let files = vec![(PathBuf::from("exact.js"), content)];
        let result = SemgrepScanner::prepare_temp_files(&files);
        assert!(result.is_ok(), "file at exact limit should be accepted");
    }

    #[tokio::test]
    async fn test_scan_batch_rejects_too_many_files() {
        let scanner = SemgrepScanner::with_binary(
            PathBuf::from("/nonexistent/semgrep"),
            std::time::Duration::from_secs(5),
        );
        // Build MAX_BATCH_FILES + 1 tiny files
        let files: Vec<(PathBuf, String)> = (0..=MAX_BATCH_FILES)
            .map(|i| (PathBuf::from(format!("f{i}.js")), "x".to_string()))
            .collect();
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("batch too large"),
            "expected 'batch too large' in error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_scan_batch_rejects_oversized_total_bytes() {
        let scanner = SemgrepScanner::with_binary(
            PathBuf::from("/nonexistent/semgrep"),
            std::time::Duration::from_secs(5),
        );
        // Two files that together exceed MAX_BATCH_SIZE_BYTES but are each
        // individually within MAX_FILE_SIZE_BYTES.  Use 60 MB each (120 MB
        // total > 100 MB limit, each < 10 MB per-file limit is NOT satisfied
        // here — use 6 MB each instead so each is under 10 MB per-file).
        let chunk = "x".repeat(6 * 1024 * 1024); // 6 MB per file
        let files: Vec<(PathBuf, String)> =
            (0..20) // 20 × 6 MB = 120 MB
                .map(|i| (PathBuf::from(format!("f{i}.js")), chunk.clone()))
                .collect();
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("batch too large"),
            "expected 'batch too large' in error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn test_scan_batch_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("fake_semgrep_slow.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 10").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let scanner = SemgrepScanner::with_binary(
            script_path,
            // Very short timeout to trigger quickly
            std::time::Duration::from_millis(100),
        );
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner
            .scan_batch(&files, &[PathBuf::from("rules.yml")])
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }
}
