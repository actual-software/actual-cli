"""Canonical IR builder.

Supports both v1 (FacetAssignment-based) and v2 (ToolMatch-based) formats.
"""

from __future__ import annotations

from collections import defaultdict
from typing import Any

from actual_logger import create_logger

from ...schemas import CanonicalIRDataV1, CanonicalIRMetadataV1, CategoryInfo, RepoKey, sha256_hex
from ...schemas.signals import ToolMatch
from ...schemas.tools_io import CanonicalIR
from ..adr_category_taxonomy import get_category_by_id
from ..facets.models import FacetAssignment

logger = create_logger(service="adr-analysis-agent", component="build-ir")

# Current taxonomy version for v2 IR
TAXONOMY_VERSION_V2 = "arch_ir_taxonomy_v2"


def build_canonical_ir(
    *,
    repo: RepoKey,
    language: str,
    content: str,
    assignment: FacetAssignment,
    extractor_id: str = "arch_ir_extractor@1",
) -> tuple[CanonicalIRMetadataV1, CanonicalIRDataV1]:
    """Build canonical IR text and metadata deterministically (v1 format).

    This is the legacy format using FacetAssignment.
    For new code, prefer build_canonical_ir_v2().
    """
    facet_blocks = assignment.facet_blocks
    lines = [
        "IRv1",
        f"FILE: {repo.file_path}",
        f"LANGUAGE: {language}",
        f"TAXONOMY: {assignment.taxonomy_version}",
        "",
    ]

    for facet in sorted(facet_blocks.keys()):
        lines.append(f"FACET: {facet}")
        entries = facet_blocks[facet]
        if entries:
            lines.extend([f"- {entry}" for entry in entries])
        else:
            lines.append("- <none>")
        lines.append("")

    canonical_text = "\n".join(lines).strip() + "\n"
    ir_hash = sha256_hex(canonical_text)
    source_hash = sha256_hex(content)

    metadata = CanonicalIRMetadataV1(
        repo=repo,
        language=language,
        taxonomy_version=assignment.taxonomy_version,
        ir_hash=ir_hash,
        source_hash=source_hash,
        extractor_id=extractor_id,
        extract_confidence=1.0 if assignment.fallback_reason is None else 0.5,
        fallback_reason=assignment.fallback_reason,
        byte_size=len(canonical_text.encode("utf-8")),
    )

    category_id = assignment.category or "24"  # Default: Internal Structuring Patterns
    category_info = get_category_by_id(category_id)
    if category_info is None:
        # Fallback for unknown category IDs
        category_info = CategoryInfo(
            id=category_id,
            name="Unknown Category",
            path="Unknown",
        )
    data = CanonicalIRDataV1(
        category=category_info,
        significance=0.5,
        confidence=0.7 if assignment.category else 0.4,
        summary_short=f"IR category: {category_info.name}",
        raw_span_ref=None,
        source_agent="ir_analysis_agent",
        canonical_text=canonical_text,
        facet_blocks=facet_blocks,
    )

    logger.info(
        "Built canonical IR",
        file_path=repo.file_path,
        ir_hash=ir_hash,
        facets=len(facet_blocks),
    )

    return metadata, data


def _aggregate_matches_by_leaf(
    matches: list[ToolMatch],
) -> dict[str, dict[str, Any]]:
    """Aggregate ToolMatch objects into facets_by_leaf_id structure.

    Returns:
        Dict mapping leaf_id to facet data with:
        - facet_slot: The primary facet slot
        - values: List of detected values
        - confidence: Aggregated confidence score
        - spans: List of evidence spans
        - rule_ids: List of rules that matched
    """
    leaf_buckets: dict[str, dict[str, Any]] = defaultdict(
        lambda: {
            "facet_slot": "",
            "values": [],
            "confidence": 0.0,
            "spans": [],
            "rule_ids": [],
            "match_count": 0,
        }
    )

    for match in matches:
        leaf_id = match.leaf_id
        bucket = leaf_buckets[leaf_id]

        # Use first facet_slot seen for this leaf
        if not bucket["facet_slot"]:
            bucket["facet_slot"] = match.facet_slot

        # Collect values
        if isinstance(match.value, str):
            if match.value not in bucket["values"]:
                bucket["values"].append(match.value)
        elif isinstance(match.value, dict):
            bucket["values"].append(match.value)

        # Track spans
        for span in match.spans:
            span_data = {
                "file_path": span.file_path,
                "start_line": span.start_line,
                "end_line": span.end_line,
                "source": span.source,
            }
            bucket["spans"].append(span_data)

        # Track rule IDs
        if match.rule_id not in bucket["rule_ids"]:
            bucket["rule_ids"].append(match.rule_id)

        # Update confidence (use max)
        bucket["confidence"] = max(bucket["confidence"], match.confidence)
        bucket["match_count"] += 1

    # Convert to final format
    result: dict[str, dict[str, Any]] = {}
    for leaf_id, bucket in leaf_buckets.items():
        result[leaf_id] = {
            "facet_slot": bucket["facet_slot"],
            "values": bucket["values"],
            "confidence": round(bucket["confidence"], 3),
            "evidence_count": len(bucket["spans"]),
            "rule_ids": bucket["rule_ids"],
            # Include spans for facet-level provenance computation
            "spans": bucket["spans"],
        }

    return result


def _build_canonical_text_v2(
    file_key: str,
    language: str,
    facets_by_leaf_id: dict[str, dict[str, Any]],
) -> str:
    """Build canonical IR text in v2 format.

    The text is ordered by leaf_id for deterministic output.
    """
    lines = [
        "IRv2",
        f"FILE: {file_key}",
        f"LANGUAGE: {language}",
        f"TAXONOMY: {TAXONOMY_VERSION_V2}",
        "",
    ]

    # Sort by leaf_id (numeric order)
    for leaf_id in sorted(facets_by_leaf_id.keys(), key=lambda x: int(x)):
        facet_data = facets_by_leaf_id[leaf_id]
        slot = facet_data["facet_slot"]
        values = facet_data["values"]
        confidence = facet_data["confidence"]

        lines.append(f"LEAF[{leaf_id}] {slot} (conf={confidence}):")
        if values:
            for val in values[:10]:  # Limit values to prevent oversized IR
                if isinstance(val, str):
                    lines.append(f"  - {val}")
                else:
                    lines.append(f"  - {val}")
        else:
            lines.append("  - <detected>")
        lines.append("")

    return "\n".join(lines).strip() + "\n"


def build_canonical_ir_v2(
    file_key: str,
    language: str,
    matches: list[ToolMatch],
    taxonomy_version: str | None = None,
) -> CanonicalIR:
    """Build canonical IR with facets_by_leaf_id structure (v2 format).

    This is the new format using ToolMatch objects from Tree-sitter/Semgrep.

    Args:
        file_key: Unique file identifier (org/repo/branch/commit/path)
        language: Detected programming language
        matches: List of ToolMatch objects from detection tools
        taxonomy_version: Optional taxonomy version override

    Returns:
        CanonicalIR with O(1) leaf_id lookup structure
    """
    taxonomy = taxonomy_version or TAXONOMY_VERSION_V2

    # Aggregate matches into leaf buckets
    facets_by_leaf_id = _aggregate_matches_by_leaf(matches)

    # Build canonical text
    canonical_text = _build_canonical_text_v2(
        file_key=file_key,
        language=language,
        facets_by_leaf_id=facets_by_leaf_id,
    )

    # Compute IR hash
    ir_hash = sha256_hex(canonical_text)

    logger.info(
        "Built canonical IR v2",
        file_key=file_key,
        language=language,
        leaf_count=len(facets_by_leaf_id),
        match_count=len(matches),
        ir_hash=ir_hash[:16],
    )

    return CanonicalIR(
        ir_text=canonical_text,
        facets_by_leaf_id=facets_by_leaf_id,
        ir_hash=ir_hash,
        taxonomy_version=taxonomy,
    )


class BuildIrTool:
    """Tool for building canonical IR from detection matches.

    Supports both v1 (legacy) and v2 (new) formats.
    """

    name = "build_ir"

    def run_v2(
        self,
        file_key: str,
        language: str,
        matches: list[ToolMatch],
    ) -> CanonicalIR:
        """Build IR using v2 format with facets_by_leaf_id.

        Args:
            file_key: Unique file identifier
            language: Programming language
            matches: Detection matches from Tree-sitter/Semgrep

        Returns:
            CanonicalIR with structured facet data
        """
        return build_canonical_ir_v2(
            file_key=file_key,
            language=language,
            matches=matches,
        )

    def run_v1(
        self,
        repo: RepoKey,
        language: str,
        content: str,
        assignment: FacetAssignment,
    ) -> tuple[CanonicalIRMetadataV1, CanonicalIRDataV1]:
        """Build IR using v1 format with facet_blocks.

        This is the legacy format for backward compatibility.
        """
        return build_canonical_ir(
            repo=repo,
            language=language,
            content=content,
            assignment=assignment,
        )
