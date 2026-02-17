"""Deterministic file analysis processor.

This module provides deterministic (non-LLM) file analysis using
Tree-sitter and Semgrep for architecture pattern detection.

Supports both single-file and batch processing modes:
- Single file: process_file() - processes one file at a time
- Batch: run_batch() - optimized batch processing with parallel Tree-sitter
  and single Semgrep invocation for 10-20x speedup

Note: Tree-sitter requires the tree-sitter-language-pack package.
"""

from __future__ import annotations

import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from actual_logger import create_logger

from .schemas.signals import ToolMatch
from .schemas.tools_io import (
    BuildContext,
    CanonicalIR,
    FileContext,
    SemgrepScanInput,
    SemgrepScanOutput,
    TreeSitterParseInput,
    TreeSitterQueryInput,
)
from .tools.ir.build_ir_tool import BuildIrTool
from .tools.semgrep.rule_pack_resolver import SemgrepRulePackResolverTool
from .tools.semgrep.scan import SemgrepScanTool

# Tree-sitter is optional - check for tree-sitter-language-pack
TREE_SITTER_AVAILABLE = False
try:
    import tree_sitter_language_pack  # noqa: F401
    TREE_SITTER_AVAILABLE = True
except ImportError:
    pass

# Import tree-sitter tools (they defer actual tree-sitter imports to runtime)
from .tools.tree_sitter.language_resolver import TreeSitterLanguageResolverTool
from .tools.tree_sitter.parse import TreeSitterParseTool
from .tools.tree_sitter.query import TreeSitterQueryTool

if TYPE_CHECKING:
    pass

logger = create_logger(service="adr-analysis-agent", component="processor")

# Retry configuration for tree-sitter and semgrep operations
PARSE_RETRY_COUNT = 3
PARSE_RETRY_BASE_DELAY = 1.0  # seconds


class TreeSitterRetryExhausted(Exception):
    """Tree-sitter parsing failed after all retries."""

    def __init__(self, file_path: str, error: str):
        self.file_path = file_path
        self.error = error
        super().__init__(f"Tree-sitter failed for {file_path} after retries: {error}")


class SemgrepRetryExhausted(Exception):
    """Semgrep scanning failed after all retries."""

    def __init__(self, file_path: str, error: str):
        self.file_path = file_path
        self.error = error
        super().__init__(f"Semgrep failed for {file_path} after retries: {error}")


@dataclass
class BatchConfig:
    """Configuration for batch processing.

    Attributes:
        batch_size: Files per Semgrep batch invocation (default 1000)
        lease_ttl: Lease TTL in seconds for claimed files (default 300)
        max_workers: Max parallel workers for Tree-sitter/IR building (default 4)
    """

    batch_size: int = 1000
    lease_ttl: int = 300
    max_workers: int = 4


@dataclass
class BatchFileFailure:
    """Records a file that failed processing after all retries.

    Attributes:
        file_path: Path of the failed file
        reason: Type of failure (e.g., 'tree_sitter_error', 'semgrep_error')
        error: Error message/details
    """

    file_path: str
    reason: str
    error: str


@dataclass
class BatchResult:
    """Result of batch processing with successes and failures.

    Attributes:
        results: Dict mapping file_path to (CanonicalIR, FileContext) for successes
        failures: List of BatchFileFailure for files that failed after retries
    """

    results: dict[str, tuple["CanonicalIR", "FileContext"]]
    failures: list[BatchFileFailure]


class ToolRegistry:
    """Central registry for all analysis tools.

    Provides lazy initialization and caching of tool instances.
    """

    def __init__(self) -> None:
        self._tree_sitter_language_resolver: TreeSitterLanguageResolverTool | None = None
        self._tree_sitter_parse: TreeSitterParseTool | None = None
        self._tree_sitter_query: TreeSitterQueryTool | None = None
        self._semgrep_rule_resolver: SemgrepRulePackResolverTool | None = None
        self._semgrep_scan: SemgrepScanTool | None = None
        self._build_ir: BuildIrTool | None = None

    @property
    def tree_sitter_available(self) -> bool:
        """Check if Tree-sitter is available."""
        return TREE_SITTER_AVAILABLE

    @property
    def tree_sitter_language_resolver(self) -> TreeSitterLanguageResolverTool:
        """Get or create Tree-sitter language resolver."""
        if self._tree_sitter_language_resolver is None:
            self._tree_sitter_language_resolver = TreeSitterLanguageResolverTool()
        return self._tree_sitter_language_resolver

    @property
    def tree_sitter_parse(self) -> TreeSitterParseTool:
        """Get or create Tree-sitter parse tool."""
        if self._tree_sitter_parse is None:
            self._tree_sitter_parse = TreeSitterParseTool()
        return self._tree_sitter_parse

    @property
    def tree_sitter_query(self) -> TreeSitterQueryTool:
        """Get or create Tree-sitter query tool."""
        if self._tree_sitter_query is None:
            self._tree_sitter_query = TreeSitterQueryTool()
        return self._tree_sitter_query

    @property
    def semgrep_rule_resolver(self) -> SemgrepRulePackResolverTool:
        """Get or create Semgrep rule pack resolver."""
        if self._semgrep_rule_resolver is None:
            self._semgrep_rule_resolver = SemgrepRulePackResolverTool()
        return self._semgrep_rule_resolver

    @property
    def semgrep_scan(self) -> SemgrepScanTool:
        """Get or create Semgrep scan tool."""
        if self._semgrep_scan is None:
            self._semgrep_scan = SemgrepScanTool()
        return self._semgrep_scan

    @property
    def build_ir(self) -> BuildIrTool:
        """Get or create IR builder tool."""
        if self._build_ir is None:
            self._build_ir = BuildIrTool()
        return self._build_ir


class DeterministicProcessor:
    """Deterministic file analysis processor.

    Runs Tree-sitter and Semgrep tools in a deterministic loop
    without LLM orchestration.

    Usage:
        processor = DeterministicProcessor()
        ir = processor.process_file(file_ctx, build_ctx)
    """

    def __init__(
        self,
        tools: ToolRegistry | None = None,
        enable_tree_sitter: bool = True,
        enable_semgrep: bool = True,
    ) -> None:
        """Initialize processor with tool registry.

        Args:
            tools: Optional pre-configured tool registry
            enable_tree_sitter: Whether to run Tree-sitter analysis
            enable_semgrep: Whether to run Semgrep analysis
        """
        self.tools = tools or ToolRegistry()
        self._enable_semgrep = enable_semgrep

        # Auto-disable tree-sitter if not available
        if enable_tree_sitter and not self.tools.tree_sitter_available:
            logger.warning(
                "Tree-sitter requested but not available. "
                "Install with: pip install tree-sitter-language-pack"
            )
            self._enable_tree_sitter = False
        else:
            self._enable_tree_sitter = enable_tree_sitter

    def _detect_build_context(self, file_ctx: FileContext) -> BuildContext:
        """Detect build context for a file based on workspace files.

        This is a simplified version that infers context from the file path.
        Full implementation would scan for build files (package.json, etc).
        """
        file_path = Path(file_ctx.file_path)
        workspace_root = str(file_path.parent)

        # Infer language and package manager from file
        lang = file_ctx.language_hint or ""
        pkg_manager = None
        build_files = []

        if lang in ("javascript", "typescript", "js", "ts"):
            pkg_manager = "npm"  # Could be pnpm, yarn
            build_files = ["package.json"]
        elif lang == "python":
            pkg_manager = "pip"  # Could be poetry, uv
            build_files = ["pyproject.toml", "setup.py", "requirements.txt"]
        elif lang == "go":
            pkg_manager = "go"
            build_files = ["go.mod"]
        elif lang == "java":
            pkg_manager = "maven"  # Could be gradle
            build_files = ["pom.xml", "build.gradle"]
        elif lang == "rust":
            pkg_manager = "cargo"
            build_files = ["Cargo.toml"]
        elif lang in ("csharp", "c#", "c_sharp", "cs"):
            pkg_manager = "dotnet"
            build_files = ["*.csproj", "*.sln"]

        return BuildContext(
            workspace_root=workspace_root,
            build_files=build_files,
            package_manager=pkg_manager,
            inferred_targets={lang: file_ctx.file_path} if lang else {},
        )

    def _run_tree_sitter(
        self,
        file_ctx: FileContext,
        build_ctx: BuildContext,
    ) -> list[ToolMatch]:
        """Run Tree-sitter parse and query tools with retry logic.

        Returns list of ToolMatch objects from Tree-sitter queries.

        Raises:
            TreeSitterRetryExhausted: If parsing fails after all retries.
        """
        matches: list[ToolMatch] = []
        last_error: str | None = None

        # Retry loop for tree-sitter parsing
        for attempt in range(1, PARSE_RETRY_COUNT + 1):
            try:
                # Parse file (tool does its own language resolution)
                parse_input = TreeSitterParseInput(file=file_ctx)
                parse_output = self.tools.tree_sitter_parse.run(parse_input)

                if parse_output.parse_error:
                    last_error = parse_output.parse_error
                    if attempt < PARSE_RETRY_COUNT:
                        delay = PARSE_RETRY_BASE_DELAY * (2 ** (attempt - 1))
                        logger.warning(
                            "Tree-sitter parse failed, retrying",
                            file_path=file_ctx.file_path,
                            attempt=attempt,
                            max_attempts=PARSE_RETRY_COUNT,
                            delay_sec=delay,
                            error=parse_output.parse_error,
                        )
                        time.sleep(delay)
                        continue
                    # All retries exhausted
                    raise TreeSitterRetryExhausted(
                        file_ctx.file_path, parse_output.parse_error
                    )

                # Parse succeeded - run queries
                query_input = TreeSitterQueryInput(
                    file=file_ctx,
                    build_context=build_ctx,
                )
                query_output = self.tools.tree_sitter_query.run(query_input)
                matches.extend(query_output.matches)

                logger.debug(
                    "Tree-sitter analysis complete",
                    file_path=file_ctx.file_path,
                    language=parse_output.tree_sitter_language_id,
                    match_count=len(matches),
                )

                return matches

            except TreeSitterRetryExhausted:
                # Re-raise our custom exception
                raise
            except Exception as e:
                last_error = str(e)
                if attempt < PARSE_RETRY_COUNT:
                    delay = PARSE_RETRY_BASE_DELAY * (2 ** (attempt - 1))
                    logger.warning(
                        "Tree-sitter exception, retrying",
                        file_path=file_ctx.file_path,
                        attempt=attempt,
                        max_attempts=PARSE_RETRY_COUNT,
                        delay_sec=delay,
                        error=str(e),
                    )
                    time.sleep(delay)
                    continue

        # All retries exhausted
        raise TreeSitterRetryExhausted(file_ctx.file_path, last_error or "Unknown error")

    def _run_semgrep(
        self,
        file_ctx: FileContext,
        build_ctx: BuildContext,
    ) -> list[ToolMatch]:
        """Run Semgrep scan with facet-specific rules and retry logic.

        Returns list of ToolMatch objects from Semgrep findings.

        Raises:
            SemgrepRetryExhausted: If scanning fails after all retries.
        """
        matches: list[ToolMatch] = []
        last_error: str | None = None

        # Resolve rule packs (no retry needed - this is local resolution)
        rule_paths = self.tools.semgrep_rule_resolver.resolve(file_ctx, build_ctx)
        if not rule_paths:
            logger.debug(
                "No Semgrep rules applicable",
                file_path=file_ctx.file_path,
            )
            return matches

        # Retry loop for semgrep scanning
        for attempt in range(1, PARSE_RETRY_COUNT + 1):
            try:
                # Run scan
                scan_input = SemgrepScanInput(
                    file=file_ctx,
                    rule_pack_ids=rule_paths,
                )
                scan_output = self.tools.semgrep_scan.run(scan_input)

                if scan_output.error:
                    last_error = scan_output.error
                    if attempt < PARSE_RETRY_COUNT:
                        delay = PARSE_RETRY_BASE_DELAY * (2 ** (attempt - 1))
                        logger.warning(
                            "Semgrep scan failed, retrying",
                            file_path=file_ctx.file_path,
                            attempt=attempt,
                            max_attempts=PARSE_RETRY_COUNT,
                            delay_sec=delay,
                            error=scan_output.error,
                        )
                        time.sleep(delay)
                        continue
                    # All retries exhausted
                    raise SemgrepRetryExhausted(file_ctx.file_path, scan_output.error)

                # Scan succeeded
                matches.extend(scan_output.matches)

                logger.debug(
                    "Semgrep analysis complete",
                    file_path=file_ctx.file_path,
                    rules_executed=scan_output.rules_executed,
                    match_count=len(matches),
                    scan_time_ms=scan_output.scan_time_ms,
                )

                return matches

            except SemgrepRetryExhausted:
                # Re-raise our custom exception
                raise
            except Exception as e:
                last_error = str(e)
                if attempt < PARSE_RETRY_COUNT:
                    delay = PARSE_RETRY_BASE_DELAY * (2 ** (attempt - 1))
                    logger.warning(
                        "Semgrep exception, retrying",
                        file_path=file_ctx.file_path,
                        attempt=attempt,
                        max_attempts=PARSE_RETRY_COUNT,
                        delay_sec=delay,
                        error=str(e),
                    )
                    time.sleep(delay)
                    continue

        # All retries exhausted
        raise SemgrepRetryExhausted(file_ctx.file_path, last_error or "Unknown error")

    def process_file(
        self,
        file_ctx: FileContext,
        build_ctx: BuildContext | None = None,
    ) -> CanonicalIR:
        """Process a single file through the deterministic pipeline.

        Pipeline steps:
        1. Tree-sitter parse + query (syntax patterns)
        2. Semgrep scan (structural semantics)
        3. Build canonical IR with facets_by_leaf_id

        Args:
            file_ctx: File context with content and metadata
            build_ctx: Optional build context (auto-detected if not provided)

        Returns:
            CanonicalIR with detected facets organized by leaf_id
        """
        # Auto-detect build context if not provided
        if build_ctx is None:
            build_ctx = self._detect_build_context(file_ctx)

        all_matches: list[ToolMatch] = []

        # Step 1: Tree-sitter analysis
        if self._enable_tree_sitter:
            try:
                ts_matches = self._run_tree_sitter(file_ctx, build_ctx)
                all_matches.extend(ts_matches)
            except TreeSitterRetryExhausted:
                # Let retry-exhausted exceptions propagate for placeholder handling
                raise
            except Exception as e:
                logger.error(
                    "Tree-sitter analysis failed",
                    file_path=file_ctx.file_path,
                    error=str(e),
                )

        # Step 2: Semgrep analysis
        if self._enable_semgrep:
            try:
                semgrep_matches = self._run_semgrep(file_ctx, build_ctx)
                all_matches.extend(semgrep_matches)
            except SemgrepRetryExhausted:
                # Let retry-exhausted exceptions propagate for placeholder handling
                raise
            except Exception as e:
                logger.error(
                    "Semgrep analysis failed",
                    file_path=file_ctx.file_path,
                    error=str(e),
                )

        # Step 3: Build canonical IR
        language = file_ctx.language_hint or "unknown"
        ir = self.tools.build_ir.run_v2(
            file_key=file_ctx.file_key,
            language=language,
            matches=all_matches,
        )

        logger.info(
            "File processed",
            file_key=file_ctx.file_key,
            language=language,
            total_matches=len(all_matches),
            leaf_count=len(ir.facets_by_leaf_id),
        )

        return ir

    def run_batch_with_failures(
        self,
        files: list[FileContext],
        build_ctx: BuildContext | None = None,
        max_workers: int = 4,
    ) -> BatchResult:
        """Process batch returning both results and failures for placeholder handling.

        Same as run_batch but returns BatchResult with tracked failures.
        Use this when you need to create placeholder components for failed files.

        Args:
            files: List of file contexts to process
            build_ctx: Optional shared build context
            max_workers: Max parallel workers for Tree-sitter (default 4)

        Returns:
            BatchResult with successful IRs and list of failures
        """
        if not files:
            return BatchResult(results={}, failures=[])

        start_time = time.time()
        failures: list[BatchFileFailure] = []
        failed_files: set[str] = set()

        # Auto-detect build contexts if not provided
        build_contexts: dict[str, BuildContext] = {}
        file_ctx_by_path: dict[str, FileContext] = {}
        for file_ctx in files:
            ctx = build_ctx or self._detect_build_context(file_ctx)
            build_contexts[file_ctx.file_path] = ctx
            file_ctx_by_path[file_ctx.file_path] = file_ctx

        # Collect all matches per file
        all_matches: dict[str, list[ToolMatch]] = {
            file_ctx.file_path: [] for file_ctx in files
        }

        # Step 1: Parallel Tree-sitter processing with failure tracking
        ts_start = time.time()
        if self._enable_tree_sitter:
            def process_tree_sitter(
                file_ctx: FileContext,
            ) -> tuple[str, list[ToolMatch], BatchFileFailure | None]:
                try:
                    ctx = build_contexts[file_ctx.file_path]
                    matches = self._run_tree_sitter(file_ctx, ctx)
                    return file_ctx.file_path, matches, None
                except TreeSitterRetryExhausted as e:
                    logger.error(
                        "Tree-sitter retry exhausted",
                        file_path=file_ctx.file_path,
                        error=e.error,
                    )
                    return file_ctx.file_path, [], BatchFileFailure(
                        file_path=file_ctx.file_path,
                        reason="tree_sitter_error",
                        error=e.error,
                    )
                except Exception as e:
                    logger.error(
                        "Tree-sitter failed unexpectedly",
                        file_path=file_ctx.file_path,
                        error=str(e),
                    )
                    return file_ctx.file_path, [], None

            with ThreadPoolExecutor(max_workers=max_workers) as executor:
                futures = {
                    executor.submit(process_tree_sitter, f): f
                    for f in files
                }
                for future in as_completed(futures):
                    file_path, matches, failure = future.result()
                    if failure:
                        failures.append(failure)
                        failed_files.add(file_path)
                    else:
                        all_matches[file_path].extend(matches)

        ts_time = (time.time() - ts_start) * 1000
        logger.debug(
            "Tree-sitter batch complete",
            file_count=len(files),
            time_ms=ts_time,
        )

        # Step 2: Batch Semgrep (only for non-failed files)
        semgrep_start = time.time()
        files_to_scan = [f for f in files if f.file_path not in failed_files]

        if self._enable_semgrep and files_to_scan:
            # Collect all rule paths needed
            all_rule_paths: set[str] = set()
            for file_ctx in files_to_scan:
                ctx = build_contexts[file_ctx.file_path]
                rule_paths = self.tools.semgrep_rule_resolver.resolve(file_ctx, ctx)
                all_rule_paths.update(rule_paths)

            if all_rule_paths:
                # Run batch scan
                semgrep_results = self.tools.semgrep_scan.run_batch(
                    files=files_to_scan,
                    rule_pack_ids=list(all_rule_paths),
                )

                # Merge results and track semgrep-specific errors
                for file_path, output in semgrep_results.items():
                    if output.error:
                        # Check if this is a persistent error (semgrep has its own retry)
                        # For now, treat any semgrep error as a failure
                        failures.append(BatchFileFailure(
                            file_path=file_path,
                            reason="semgrep_error",
                            error=output.error,
                        ))
                        failed_files.add(file_path)
                    else:
                        all_matches[file_path].extend(output.matches)

        semgrep_time = (time.time() - semgrep_start) * 1000
        logger.debug(
            "Semgrep batch complete",
            file_count=len(files_to_scan),
            time_ms=semgrep_time,
        )

        # Step 3: Build IRs (only for non-failed files)
        ir_start = time.time()
        successful_results: dict[str, tuple[CanonicalIR, FileContext]] = {}
        files_to_build = [f for f in files if f.file_path not in failed_files]

        def build_ir_for_file(
            file_ctx: FileContext, matches: list[ToolMatch]
        ) -> tuple[str, CanonicalIR]:
            language = file_ctx.language_hint or "unknown"
            try:
                ir = self.tools.build_ir.run_v2(
                    file_key=file_ctx.file_key,
                    language=language,
                    matches=matches,
                )
                return file_ctx.file_path, ir
            except Exception as e:
                logger.error(
                    "IR build failed",
                    file_path=file_ctx.file_path,
                    error=str(e),
                )
                return file_ctx.file_path, CanonicalIR(
                    ir_text=f"IRv2\nFILE: {file_ctx.file_key}\nERROR: {e}\n",
                    facets_by_leaf_id={},
                    ir_hash="",
                    taxonomy_version="arch_ir_taxonomy_v2",
                )

        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            futures = {
                executor.submit(
                    build_ir_for_file, f, all_matches[f.file_path]
                ): f
                for f in files_to_build
            }
            for future in as_completed(futures):
                file_path, ir = future.result()
                file_ctx = file_ctx_by_path[file_path]
                successful_results[file_path] = (ir, file_ctx)

        ir_time = (time.time() - ir_start) * 1000
        total_time = (time.time() - start_time) * 1000

        logger.info(
            "Batch processing complete",
            total_files=len(files),
            successful=len(successful_results),
            failures=len(failures),
            total_time_ms=total_time,
            tree_sitter_time_ms=ts_time,
            semgrep_time_ms=semgrep_time,
            ir_build_time_ms=ir_time,
            avg_time_per_file_ms=total_time / len(files) if files else 0,
        )

        return BatchResult(results=successful_results, failures=failures)

    def run_batch(
        self,
        files: list[FileContext],
        build_ctx: BuildContext | None = None,
        max_workers: int = 4,
    ) -> list[CanonicalIR]:
        """Process a batch of files with optimized batch processing.

        Optimizations:
        - Parallel Tree-sitter processing using ThreadPoolExecutor
        - Single Semgrep invocation for all files (10-20x faster)
        - Parallel IR building

        Note: This method does not return failure information. Use
        run_batch_with_failures() if you need to handle failed files.

        Args:
            files: List of file contexts to process
            build_ctx: Optional shared build context
            max_workers: Max parallel workers for Tree-sitter (default 4)

        Returns:
            List of CanonicalIR objects (one per file, same order as input)
        """
        batch_result = self.run_batch_with_failures(files, build_ctx, max_workers)

        # Convert BatchResult to list, preserving order and creating error IRs for failures
        file_order = {f.file_path: i for i, f in enumerate(files)}
        results: list[CanonicalIR | None] = [None] * len(files)

        # Add successful results
        for file_path, (ir, _) in batch_result.results.items():
            idx = file_order[file_path]
            results[idx] = ir

        # Create error IRs for failures
        for failure in batch_result.failures:
            idx = file_order[failure.file_path]
            file_ctx = files[idx]
            results[idx] = CanonicalIR(
                ir_text=f"IRv2\nFILE: {file_ctx.file_key}\nERROR: {failure.reason}: {failure.error}\n",
                facets_by_leaf_id={},
                ir_hash="",
                taxonomy_version="arch_ir_taxonomy_v2",
            )

        # Fill any gaps (should not happen)
        for i, r in enumerate(results):
            if r is None:
                results[i] = CanonicalIR(
                    ir_text=f"IRv2\nFILE: {files[i].file_key}\nERROR: Unknown\n",
                    facets_by_leaf_id={},
                    ir_hash="",
                    taxonomy_version="arch_ir_taxonomy_v2",
                )

        return [r for r in results if r is not None]


def create_processor(
    enable_tree_sitter: bool = True,
    enable_semgrep: bool = True,
) -> DeterministicProcessor:
    """Factory function to create a configured processor.

    Args:
        enable_tree_sitter: Whether to enable Tree-sitter analysis
        enable_semgrep: Whether to enable Semgrep analysis

    Returns:
        Configured DeterministicProcessor instance
    """
    return DeterministicProcessor(
        enable_tree_sitter=enable_tree_sitter,
        enable_semgrep=enable_semgrep,
    )
