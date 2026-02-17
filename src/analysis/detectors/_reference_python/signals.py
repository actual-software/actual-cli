"""Architecture signal models for tool matches and evidence spans.

These models are used by Tree-sitter, Semgrep, and other detection tools
to report findings in a unified format.
"""

from __future__ import annotations

from typing import Any, Literal

from pydantic import BaseModel, ConfigDict, Field


class EvidenceSpan(BaseModel):
    """A span of source code that provides evidence for a facet detection.

    Spans are used to link architecture signals back to specific
    locations in the source code.
    """

    model_config = ConfigDict(extra="forbid")

    file_path: str = Field(description="Path to the source file")
    start_byte: int = Field(ge=0, description="Start byte offset in the file")
    end_byte: int = Field(ge=0, description="End byte offset in the file")
    start_line: int | None = Field(
        default=None, ge=1, description="1-indexed start line number"
    )
    end_line: int | None = Field(
        default=None, ge=1, description="1-indexed end line number"
    )
    source: Literal[
        "tree_sitter",
        "semgrep",
        "scip",
        "python_ast",
        "sqlglot",
        "regex",
        "config",
    ] = Field(description="The tool that produced this span")


class ToolMatch(BaseModel):
    """A single match from a detection tool mapped to the facet taxonomy.

    Each match represents a detected architecture signal that maps to
    a specific facet slot and leaf category.
    """

    model_config = ConfigDict(extra="forbid")

    rule_id: str = Field(
        description="Unique identifier for the detection rule (e.g., 'ts.public_function', 'semgrep.express_route')"
    )
    facet_slot: str = Field(
        description="Target facet slot path (e.g., 'api.public.protocols', 'libs.core.detected')"
    )
    leaf_id: str = Field(
        description="Leaf category ID from taxonomy (e.g., '25' for Public/External APIs)"
    )
    value: str | dict[str, Any] = Field(
        description="Detected value (e.g., function name, library name, or structured data)"
    )
    confidence: float = Field(
        ge=0.0, le=1.0, description="Detection confidence score"
    )
    spans: list[EvidenceSpan] = Field(
        default_factory=list, description="Source code spans providing evidence"
    )
    raw: dict[str, Any] | None = Field(
        default=None, description="Raw tool output for debugging"
    )


class ArchitectureSignals(BaseModel):
    """Collection of architecture signals for a single file.

    This is the output of the detection phase, before normalization
    into the canonical IR format.
    """

    model_config = ConfigDict(extra="forbid")

    file_key: str = Field(
        description="Stable file key: repo_unique_id:branch:commit_sha:file_path"
    )
    matches: list[ToolMatch] = Field(
        default_factory=list, description="All detected architecture signals"
    )

    def matches_by_leaf_id(self) -> dict[str, list[ToolMatch]]:
        """Group matches by leaf_id for O(1) category lookup."""
        result: dict[str, list[ToolMatch]] = {}
        for match in self.matches:
            result.setdefault(match.leaf_id, []).append(match)
        return result

    def matches_by_facet_slot(self) -> dict[str, list[ToolMatch]]:
        """Group matches by facet_slot."""
        result: dict[str, list[ToolMatch]] = {}
        for match in self.matches:
            result.setdefault(match.facet_slot, []).append(match)
        return result
