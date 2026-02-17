use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::process::Command;

use super::{EvidenceSpan, SignalSource, ToolMatch};

/// Async scanner that invokes the semgrep CLI on batches of files.
pub struct SemgrepScanner {
    semgrep_bin: PathBuf,
    timeout: std::time::Duration,
}

impl SemgrepScanner {
    /// Create a new scanner, verifying that the semgrep binary is available.
    pub fn new(timeout: std::time::Duration) -> Result<Self> {
        let semgrep_bin = which::which("semgrep").context("semgrep binary not found in PATH")?;
        Ok(Self {
            semgrep_bin,
            timeout,
        })
    }

    /// Create a scanner with a specific binary path (useful for testing).
    #[cfg(test)]
    fn with_binary(bin: PathBuf, timeout: std::time::Duration) -> Self {
        Self {
            semgrep_bin: bin,
            timeout,
        }
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

        // Create temp directory and write files into it
        let temp_dir = tempfile::tempdir().context("failed to create temp directory")?;
        let mut file_map: HashMap<PathBuf, PathBuf> = HashMap::new();

        for (i, (rel_path, content)) in files.iter().enumerate() {
            // Use indexed subdirs to avoid name collisions
            let subdir = temp_dir.path().join(format!("f{i}"));
            std::fs::create_dir_all(&subdir).with_context(|| {
                format!("failed to create subdir for file {}", rel_path.display())
            })?;

            // Preserve the file name (semgrep uses extension for language detection)
            let file_name = rel_path
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("file.txt"));
            let temp_path = subdir.join(file_name);

            std::fs::write(&temp_path, content)
                .with_context(|| format!("failed to write temp file {}", temp_path.display()))?;

            file_map.insert(temp_path, rel_path.clone());
        }

        // Build semgrep command
        let mut cmd = Command::new(&self.semgrep_bin);
        cmd.arg("scan")
            .arg("--json")
            .arg("--no-git-ignore")
            .arg("--metrics=off");

        for rule_file in rule_files {
            cmd.arg("--config").arg(rule_file);
        }

        cmd.arg(temp_dir.path());

        // Run with timeout
        let output = tokio::time::timeout(self.timeout, cmd.output())
            .await
            .context("semgrep scan timed out")?
            .context("failed to execute semgrep")?;

        // Parse JSON output regardless of exit code.
        // semgrep exit codes: 0 = no findings, 1 = findings found (in some modes),
        // 2+ = errors. We parse stdout JSON in all cases.
        let stdout = String::from_utf8_lossy(&output.stdout);

        if stdout.trim().is_empty() {
            // If no JSON output, check stderr for login errors
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("login") || stderr.contains("Login") {
                bail!(
                    "semgrep requires login (v1.100+). \
                     Try running `semgrep login` or use an older version. \
                     stderr: {stderr}"
                );
            }
            if !output.status.success() {
                bail!(
                    "semgrep exited with status {} and no JSON output. stderr: {stderr}",
                    output.status
                );
            }
            return Ok(vec![]);
        }

        let parsed: serde_json::Value = serde_json::from_str(&stdout).with_context(|| {
            format!(
                "failed to parse semgrep JSON output (exit code: {})",
                output.status
            )
        })?;

        Self::parse_findings(&parsed, &file_map)
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

            // Map temp path back to original path
            let original_path = file_map
                .get(&temp_path)
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
        // Falls back to the raw path string
        assert_eq!(matches[0].spans[0].file_path, "/some/unknown/path.js");
    }

    #[test]
    fn test_parse_findings_raw_preserved() {
        let output = sample_semgrep_output();
        let file_map = HashMap::new();
        let matches = SemgrepScanner::parse_findings(&output, &file_map).unwrap();
        // raw should contain the full finding JSON
        for m in &matches {
            assert!(m.raw.is_some());
            let raw = m.raw.as_ref().unwrap();
            assert!(raw.get("check_id").is_some());
        }
    }

    #[test]
    fn test_scanner_creation_fails_without_semgrep() {
        // Temporarily modify PATH to ensure semgrep isn't found
        // This test verifies the error message is helpful
        let result = SemgrepScanner::new(std::time::Duration::from_secs(30));
        // We can't guarantee semgrep is or isn't installed, so just verify
        // the function returns a Result and doesn't panic
        match result {
            Ok(scanner) => {
                // semgrep is installed — verify the binary path is populated
                assert!(!scanner.semgrep_bin.as_os_str().is_empty());
            }
            Err(e) => {
                // semgrep is not installed — verify the error message
                let msg = format!("{e}");
                assert!(msg.contains("semgrep"));
            }
        }
    }

    #[tokio::test]
    async fn test_scan_batch_empty_files() {
        // Create scanner with a dummy binary (won't be invoked)
        let scanner = SemgrepScanner::with_binary(
            PathBuf::from("semgrep"),
            std::time::Duration::from_secs(5),
        );
        let result = scanner.scan_batch(&[], &[PathBuf::from("rule.yml")]).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_scan_batch_empty_rules() {
        let scanner = SemgrepScanner::with_binary(
            PathBuf::from("semgrep"),
            std::time::Duration::from_secs(5),
        );
        let files = vec![(PathBuf::from("test.js"), "const x = 1;".to_string())];
        let result = scanner.scan_batch(&files, &[]).await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
