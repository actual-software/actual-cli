"""Semgrep scan tool.

Runs Semgrep scans against source files and extracts architecture signals.
Supports both single-file and batch scanning modes.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

from actual_logger import create_logger

from ...schemas.signals import EvidenceSpan, ToolMatch
from ...schemas.tools_io import FileContext, SemgrepScanInput, SemgrepScanOutput

logger = create_logger(service="adr-analysis-agent", component="semgrep-scan")

# Default semgrep binary
DEFAULT_SEMGREP_BIN = os.getenv("SEMGREP_BIN", "semgrep")

# Timeout for semgrep execution (seconds)
SEMGREP_TIMEOUT = int(os.getenv("SEMGREP_TIMEOUT", "60"))


def _byte_offset_to_line(content: str, offset: int) -> int:
    """Convert byte offset to 1-indexed line number.

    Args:
        content: Source file content
        offset: Byte offset in the content

    Returns:
        1-indexed line number
    """
    if offset <= 0:
        return 1
    return content[:offset].count("\n") + 1


class SemgrepScanTool:
    """Runs Semgrep scans to detect architecture patterns.

    This tool executes Semgrep with facet-specific rule packs and
    parses the JSON output into ToolMatch objects.
    """

    name = "semgrep_scan"

    def __init__(
        self,
        semgrep_bin: str | None = None,
        timeout: int | None = None,
    ) -> None:
        self._semgrep_bin = semgrep_bin or DEFAULT_SEMGREP_BIN
        self._timeout = timeout or SEMGREP_TIMEOUT

    def _check_semgrep_available(self) -> bool:
        """Check if semgrep is available."""
        try:
            result = subprocess.run(
                [self._semgrep_bin, "--version"],
                capture_output=True,
                text=True,
                timeout=10,
            )
            return result.returncode == 0
        except (subprocess.TimeoutExpired, FileNotFoundError):
            return False

    def _parse_finding(
        self,
        finding: dict[str, Any],
        file_path: str,
        source_content: str | None = None,
    ) -> ToolMatch | None:
        """Parse a Semgrep finding into a ToolMatch.

        Args:
            finding: Raw finding from Semgrep JSON output
            file_path: Path to the scanned file
            source_content: Optional source content to extract matched code
                when Semgrep returns "requires login" (v1.100+ without login)
        """
        try:
            check_id = finding.get("check_id", "unknown")
            extra = finding.get("extra", {})
            metadata = extra.get("metadata", {})

            # Extract facet mapping from rule metadata
            facet_slot = metadata.get("facet_slot", "unknown")
            leaf_id = str(metadata.get("leaf_id", "0"))
            confidence = float(metadata.get("confidence", 0.7))

            # Build evidence span first - we need this for extraction
            start = finding.get("start", {})
            end = finding.get("end", {})
            start_offset = start.get("offset", 0)
            end_offset = end.get("offset", 0)

            # Get the matched value - prefer the actual matched source code
            # The 'lines' field contains the matched source code lines
            # Note: Semgrep 1.100+ returns "requires login" if not logged in
            value = extra.get("lines", "")

            # Work around "requires login" by extracting from source content
            if (not value or value == "requires login") and source_content:
                try:
                    # Extract the matched code using byte offsets
                    value = source_content[start_offset:end_offset].strip()
                except (IndexError, TypeError):
                    pass

            if not value or value == "requires login":
                # Fall back to rule ID if extraction failed
                value = check_id

            # Get line info from Semgrep, fallback to computing from byte offsets
            start_line = start.get("line")
            end_line = end.get("line")

            # Compute line info from byte offsets if missing
            if (start_line is None or end_line is None) and source_content:
                start_line = start_line or _byte_offset_to_line(source_content, start_offset)
                end_line = end_line or _byte_offset_to_line(source_content, end_offset)

            span = EvidenceSpan(
                file_path=file_path,
                start_byte=start_offset,
                end_byte=end_offset,
                start_line=start_line,
                end_line=end_line,
                source="semgrep",
            )

            return ToolMatch(
                rule_id=check_id,
                facet_slot=facet_slot,
                leaf_id=leaf_id,
                value=value if isinstance(value, (str, dict)) else str(value),
                confidence=confidence,
                spans=[span],
                raw=finding,
            )

        except Exception as e:
            logger.warning(
                "Failed to parse semgrep finding",
                error=str(e),
            )
            return None

    def run(self, inp: SemgrepScanInput) -> SemgrepScanOutput:
        """Run Semgrep scan on a file.

        Args:
            inp: Input containing file context and rule pack IDs

        Returns:
            SemgrepScanOutput with detected matches
        """
        file_ctx = inp.file
        rule_pack_ids = inp.rule_pack_ids

        if not rule_pack_ids:
            logger.debug(
                "No rule packs to run",
                file_path=file_ctx.file_path,
            )
            return SemgrepScanOutput(
                matches=[],
                rules_executed=0,
                scan_time_ms=0.0,
            )

        # Check if semgrep is available
        if not self._check_semgrep_available():
            logger.warning(
                "Semgrep not available",
                semgrep_bin=self._semgrep_bin,
            )
            return SemgrepScanOutput(
                matches=[],
                rules_executed=0,
                scan_time_ms=0.0,
                error="Semgrep not available",
            )

        start_time = time.time()

        try:
            # Write file content to temp file
            with tempfile.TemporaryDirectory() as tmpdir:
                # Create file with original extension for language detection
                file_name = os.path.basename(file_ctx.file_path)
                temp_file = os.path.join(tmpdir, file_name)
                with open(temp_file, "w", encoding="utf-8") as f:
                    f.write(file_ctx.content_utf8)

                # Build semgrep command
                cmd = [
                    self._semgrep_bin,
                    "scan",
                    "--json",
                    "--no-git-ignore",
                    "--metrics=off",
                ]

                # Add rule configs
                for rule_path in rule_pack_ids:
                    cmd.extend(["--config", rule_path])

                # Add target file
                cmd.append(temp_file)

                logger.debug(
                    "Running semgrep",
                    command=" ".join(cmd),
                    file_path=file_ctx.file_path,
                )

                # Run semgrep
                result = subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                    timeout=self._timeout,
                    cwd=tmpdir,
                )

                scan_time_ms = (time.time() - start_time) * 1000

                # Semgrep return codes (per https://semgrep.dev/docs/cli-reference):
                # 0 = success (no findings, or findings without --error flag)
                # 1 = findings found (with --error flag)
                # 2 = execution failed
                # We don't use --error flag, so expect 0 on success
                if result.returncode != 0:
                    error_msg = result.stderr[:1000] if result.stderr else "Unknown error"
                    if not error_msg.strip():
                        error_msg = result.stdout[:1000] if result.stdout else "Unknown error (no output)"
                    logger.error(
                        "Semgrep scan failed",
                        returncode=result.returncode,
                        error=error_msg,
                        stdout_preview=result.stdout[:500] if result.stdout else None,
                    )
                    return SemgrepScanOutput(
                        matches=[],
                        rules_executed=0,
                        scan_time_ms=scan_time_ms,
                        error=error_msg,
                    )

                # Parse JSON output
                try:
                    data = json.loads(result.stdout)
                except json.JSONDecodeError as e:
                    logger.error(
                        "Failed to parse semgrep output",
                        error=str(e),
                    )
                    return SemgrepScanOutput(
                        matches=[],
                        rules_executed=0,
                        scan_time_ms=scan_time_ms,
                        error=f"JSON parse error: {e}",
                    )

                # Extract matches
                matches: list[ToolMatch] = []
                findings = data.get("results", [])

                for finding in findings:
                    match = self._parse_finding(
                        finding,
                        file_ctx.file_path,
                        source_content=file_ctx.content_utf8,
                    )
                    if match:
                        matches.append(match)

                # Count rules from the response
                rules_executed = len(data.get("paths", {}).get("scanned", []))

                logger.info(
                    "Semgrep scan complete",
                    file_path=file_ctx.file_path,
                    findings_count=len(findings),
                    matches_count=len(matches),
                    scan_time_ms=scan_time_ms,
                )

                return SemgrepScanOutput(
                    matches=matches,
                    rules_executed=rules_executed,
                    scan_time_ms=scan_time_ms,
                )

        except subprocess.TimeoutExpired:
            scan_time_ms = (time.time() - start_time) * 1000
            logger.error(
                "Semgrep scan timed out",
                file_path=file_ctx.file_path,
                timeout=self._timeout,
            )
            return SemgrepScanOutput(
                matches=[],
                rules_executed=0,
                scan_time_ms=scan_time_ms,
                error=f"Timeout after {self._timeout}s",
            )

        except Exception as e:
            scan_time_ms = (time.time() - start_time) * 1000
            logger.error(
                "Semgrep scan error",
                file_path=file_ctx.file_path,
                error=str(e),
            )
            return SemgrepScanOutput(
                matches=[],
                rules_executed=0,
                scan_time_ms=scan_time_ms,
                error=str(e),
            )

    def run_batch(
        self,
        files: list[FileContext],
        rule_pack_ids: list[str],
    ) -> dict[str, SemgrepScanOutput]:
        """Run Semgrep scan on multiple files in a single invocation.

        This is significantly faster than scanning files individually because
        Semgrep startup time (~2-3s) is amortized across all files.

        Args:
            files: List of file contexts to scan
            rule_pack_ids: Rule packs to apply to all files

        Returns:
            Dict mapping file_path to SemgrepScanOutput for each file
        """
        results: dict[str, SemgrepScanOutput] = {}

        if not files:
            return results

        if not rule_pack_ids:
            logger.debug("No rule packs to run for batch")
            for file_ctx in files:
                results[file_ctx.file_path] = SemgrepScanOutput(
                    matches=[],
                    rules_executed=0,
                    scan_time_ms=0.0,
                )
            return results

        # Check if semgrep is available
        if not self._check_semgrep_available():
            logger.warning("Semgrep not available", semgrep_bin=self._semgrep_bin)
            for file_ctx in files:
                results[file_ctx.file_path] = SemgrepScanOutput(
                    matches=[],
                    rules_executed=0,
                    scan_time_ms=0.0,
                    error="Semgrep not available",
                )
            return results

        start_time = time.time()

        try:
            with tempfile.TemporaryDirectory() as tmpdir:
                # Write all files to temp directory, preserving unique paths
                # Use hash to create unique subdirs if paths collide
                temp_to_original: dict[str, FileContext] = {}
                file_contents_map: dict[str, str] = {}

                for idx, file_ctx in enumerate(files):
                    # Create unique path using index prefix to avoid collisions
                    file_name = os.path.basename(file_ctx.file_path)
                    # Use subdirectory per file to ensure unique paths
                    subdir = os.path.join(tmpdir, f"f{idx}")
                    os.makedirs(subdir, exist_ok=True)
                    temp_file = os.path.join(subdir, file_name)

                    with open(temp_file, "w", encoding="utf-8") as f:
                        f.write(file_ctx.content_utf8)

                    temp_to_original[temp_file] = file_ctx
                    file_contents_map[temp_file] = file_ctx.content_utf8

                # Build semgrep command for batch scan
                cmd = [
                    self._semgrep_bin,
                    "scan",
                    "--json",
                    "--no-git-ignore",
                    "--metrics=off",
                ]

                # Add rule configs
                for rule_path in rule_pack_ids:
                    cmd.extend(["--config", rule_path])

                # Scan entire temp directory
                cmd.append(tmpdir)

                logger.debug(
                    "Running batch semgrep",
                    file_count=len(files),
                    command=" ".join(cmd[:6]) + " ...",  # Truncate for logging
                )

                # Run semgrep
                result = subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                    timeout=self._timeout * 2,  # Allow more time for batch
                    cwd=tmpdir,
                )

                scan_time_ms = (time.time() - start_time) * 1000

                # Semgrep return codes (per https://semgrep.dev/docs/cli-reference):
                # 0 = success, 1 = findings with --error flag, 2 = execution failed
                # We don't use --error flag, so expect 0 on success
                if result.returncode != 0:
                    error_msg = result.stderr[:1000] if result.stderr else "Unknown error"
                    if not error_msg.strip():
                        error_msg = result.stdout[:1000] if result.stdout else "Unknown error"
                    logger.error(
                        "Batch semgrep scan failed",
                        returncode=result.returncode,
                        error=error_msg,
                    )
                    for file_ctx in files:
                        results[file_ctx.file_path] = SemgrepScanOutput(
                            matches=[],
                            rules_executed=0,
                            scan_time_ms=scan_time_ms / len(files),
                            error=error_msg,
                        )
                    return results

                # Parse JSON output
                try:
                    data = json.loads(result.stdout)
                except json.JSONDecodeError as e:
                    logger.error("Failed to parse batch semgrep output", error=str(e))
                    for file_ctx in files:
                        results[file_ctx.file_path] = SemgrepScanOutput(
                            matches=[],
                            rules_executed=0,
                            scan_time_ms=scan_time_ms / len(files),
                            error=f"JSON parse error: {e}",
                        )
                    return results

                # Group findings by temp file path
                findings_by_temp: dict[str, list[dict]] = {}
                for finding in data.get("results", []):
                    temp_path = finding.get("path", "")
                    if temp_path not in findings_by_temp:
                        findings_by_temp[temp_path] = []
                    findings_by_temp[temp_path].append(finding)

                # Map findings back to original files
                for temp_path, file_ctx in temp_to_original.items():
                    file_findings = findings_by_temp.get(temp_path, [])
                    matches: list[ToolMatch] = []
                    source_content = file_contents_map.get(temp_path)

                    for finding in file_findings:
                        # Replace temp path with original path in finding
                        match = self._parse_finding(
                            finding,
                            file_ctx.file_path,
                            source_content=source_content,
                        )
                        if match:
                            matches.append(match)

                    results[file_ctx.file_path] = SemgrepScanOutput(
                        matches=matches,
                        rules_executed=len(rule_pack_ids),
                        scan_time_ms=scan_time_ms / len(files),  # Approximate per-file
                    )

                # Handle files with no findings
                for file_ctx in files:
                    if file_ctx.file_path not in results:
                        results[file_ctx.file_path] = SemgrepScanOutput(
                            matches=[],
                            rules_executed=len(rule_pack_ids),
                            scan_time_ms=scan_time_ms / len(files),
                        )

                total_matches = sum(len(r.matches) for r in results.values())
                logger.info(
                    "Batch semgrep scan complete",
                    file_count=len(files),
                    total_findings=total_matches,
                    scan_time_ms=scan_time_ms,
                    avg_time_per_file_ms=scan_time_ms / len(files),
                )

                return results

        except subprocess.TimeoutExpired:
            scan_time_ms = (time.time() - start_time) * 1000
            logger.error(
                "Batch semgrep scan timed out",
                file_count=len(files),
                timeout=self._timeout * 2,
            )
            for file_ctx in files:
                results[file_ctx.file_path] = SemgrepScanOutput(
                    matches=[],
                    rules_executed=0,
                    scan_time_ms=scan_time_ms / len(files),
                    error=f"Timeout after {self._timeout * 2}s",
                )
            return results

        except Exception as e:
            scan_time_ms = (time.time() - start_time) * 1000
            logger.error(
                "Batch semgrep scan error",
                file_count=len(files),
                error=str(e),
            )
            for file_ctx in files:
                results[file_ctx.file_path] = SemgrepScanOutput(
                    matches=[],
                    rules_executed=0,
                    scan_time_ms=scan_time_ms / len(files),
                    error=str(e),
                )
            return results
