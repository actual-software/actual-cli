"""Tool input/output contracts for the ADR analysis pipeline.

All tool I/O is validated with Pydantic v2 models for type safety
and serialization consistency.
"""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field, computed_field

from .signals import ToolMatch


# =============================================================================
# Context Models
# =============================================================================


class FileContext(BaseModel):
    """Context for a single file being analyzed.

    This is the primary input to most analysis tools.
    """

    model_config = ConfigDict(extra="forbid")

    org_id: str = Field(description="Organization ID")
    repo_unique_id: str = Field(description="Unique repository identifier")
    branch: str = Field(description="Git branch name")
    commit_sha: str = Field(description="Git commit SHA")
    file_path: str = Field(description="Path to the file within the repo")
    content_utf8: str = Field(description="UTF-8 encoded file content")
    language_hint: str | None = Field(
        default=None, description="Hint for the programming language"
    )

    @computed_field
    @property
    def file_key(self) -> str:
        """Generate stable file key for deduplication."""
        return f"{self.repo_unique_id}:{self.branch}:{self.commit_sha}:{self.file_path}"


class BuildContext(BaseModel):
    """Build system context for a file.

    Provides information about the workspace, build files, and package manager
    that can help with more accurate detection.
    """

    model_config = ConfigDict(extra="forbid")

    workspace_root: str | None = Field(
        default=None, description="Workspace root directory path"
    )
    build_files: list[str] = Field(
        default_factory=list,
        description="Paths to detected build files (package.json, pyproject.toml, etc.)",
    )
    package_manager: str | None = Field(
        default=None,
        description="Detected package manager (npm, pnpm, poetry, cargo, etc.)",
    )
    inferred_targets: dict[str, str] = Field(
        default_factory=dict,
        description="Inferred language/platform targets (e.g., {'TypeScript': '5.4', 'Java': '17'})",
    )


# =============================================================================
# Tree-sitter Tool I/O
# =============================================================================


class TreeSitterParseInput(BaseModel):
    """Input for tree-sitter parse tool."""

    model_config = ConfigDict(extra="forbid")

    file: FileContext = Field(description="File context to parse")


class TreeSitterParseOutput(BaseModel):
    """Output from tree-sitter parse tool."""

    model_config = ConfigDict(extra="forbid")

    language: str = Field(description="Detected language name")
    tree_sitter_language_id: str = Field(
        description="Tree-sitter language grammar ID"
    )
    node_index: dict[str, dict[str, Any]] = Field(
        description="Compact node index with spans and key node kinds"
    )
    parse_error: str | None = Field(
        default=None, description="Parse error message if parsing failed"
    )


class TreeSitterQueryInput(BaseModel):
    """Input for tree-sitter query tool."""

    model_config = ConfigDict(extra="forbid")

    file: FileContext = Field(description="File context to query")
    build_context: BuildContext = Field(description="Build context for the file")
    query_pack_ids: list[str] | None = Field(
        default=None,
        description="Optional specific query pack IDs to run (defaults to all for language)",
    )


class TreeSitterQueryOutput(BaseModel):
    """Output from tree-sitter query tool."""

    model_config = ConfigDict(extra="forbid")

    matches: list[ToolMatch] = Field(
        default_factory=list, description="Detected architecture signals"
    )
    queries_executed: int = Field(
        default=0, description="Number of queries executed"
    )


# =============================================================================
# Semgrep Tool I/O
# =============================================================================


class SemgrepScanInput(BaseModel):
    """Input for semgrep scan tool."""

    model_config = ConfigDict(extra="forbid")

    file: FileContext = Field(description="File context to scan")
    rule_pack_ids: list[str] = Field(
        description="List of rule pack IDs to apply"
    )


class SemgrepScanOutput(BaseModel):
    """Output from semgrep scan tool."""

    model_config = ConfigDict(extra="forbid")

    matches: list[ToolMatch] = Field(
        default_factory=list, description="Detected architecture signals"
    )
    rules_executed: int = Field(default=0, description="Number of rules executed")
    scan_time_ms: float = Field(
        default=0.0, description="Scan time in milliseconds"
    )
    error: str | None = Field(
        default=None, description="Error message if scan failed"
    )


# =============================================================================
# Build Context Tool I/O
# =============================================================================


class LoadBuildContextInput(BaseModel):
    """Input for build context loader tool."""

    model_config = ConfigDict(extra="forbid")

    file: FileContext = Field(description="File context to load build context for")


class LoadBuildContextOutput(BaseModel):
    """Output from build context loader tool."""

    model_config = ConfigDict(extra="forbid")

    build_context: BuildContext = Field(description="Loaded build context")
    detection_method: str = Field(
        default="heuristic",
        description="Method used to detect build context",
    )


# =============================================================================
# IR Builder Tool I/O
# =============================================================================


class BuildIRInput(BaseModel):
    """Input for IR builder tool."""

    model_config = ConfigDict(extra="forbid")

    file_key: str = Field(description="Stable file key")
    matches: list[ToolMatch] = Field(
        description="All detected architecture signals"
    )
    language: str = Field(description="Programming language")
    taxonomy_version: str = Field(
        default="v2", description="Taxonomy version to use"
    )


class CanonicalIR(BaseModel):
    """Output from IR builder - the canonical intermediate representation.

    This is the primary output format with O(1) category lookup via
    facets_by_leaf_id.
    """

    model_config = ConfigDict(extra="forbid")

    ir_text: str = Field(description="Canonical text representation")
    facets_by_leaf_id: dict[str, dict[str, Any]] = Field(
        description="Facets grouped by leaf category ID for O(1) lookup"
    )
    ir_hash: str = Field(description="SHA256 hash of ir_text")
    taxonomy_version: str = Field(description="Taxonomy version used")
    match_count: int = Field(default=0, description="Total number of matches")
    leaf_count: int = Field(
        default=0, description="Number of unique leaf categories with matches"
    )


# =============================================================================
# SQLGlot Tool I/O
# =============================================================================


class SQLGlotParseInput(BaseModel):
    """Input for SQLGlot parse tool."""

    model_config = ConfigDict(extra="forbid")

    file: FileContext = Field(description="SQL file context to parse")
    dialect: str | None = Field(
        default=None,
        description="SQL dialect (postgres, mysql, sqlite). Auto-detects if not specified",
    )


class SQLGlotParseOutput(BaseModel):
    """Output from SQLGlot parse tool."""

    model_config = ConfigDict(extra="forbid")

    matches: list[ToolMatch] = Field(
        default_factory=list, description="Detected SQL patterns"
    )
    tables_found: int = Field(default=0, description="Number of tables found")
    dialect_used: str | None = Field(
        default=None, description="SQL dialect used for parsing"
    )
    parse_error: str | None = Field(
        default=None, description="Parse error if any"
    )
