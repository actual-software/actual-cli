"""Tree-sitter query tool.

Runs tree-sitter queries against parsed syntax trees to extract
architecture signals.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import TYPE_CHECKING, Any

from actual_logger import create_logger

from ...schemas.signals import EvidenceSpan, ToolMatch
from ...schemas.tools_io import TreeSitterQueryInput, TreeSitterQueryOutput
from .language_resolver import TreeSitterLanguageResolverTool
from .parse import TreeSitterParseTool

if TYPE_CHECKING:
    from tree_sitter import Query

logger = create_logger(service="adr-analysis-agent", component="tree-sitter-query")

# Directory containing query pack files
QUERY_PACKS_DIR = Path(__file__).parent.parent.parent / "detectors" / "tree_sitter_queries"


class QueryPackDefinition:
    """Definition of a query pack with its queries and metadata."""

    def __init__(
        self,
        pack_id: str,
        language_id: str,
        queries: list[dict[str, Any]],
    ) -> None:
        self.pack_id = pack_id
        self.language_id = language_id
        self.queries = queries


class TreeSitterQueryTool:
    """Runs tree-sitter queries to extract architecture signals.

    This tool loads query packs for each language and runs them against
    parsed syntax trees to extract architecture signals.
    """

    name = "tree_sitter_query"

    def __init__(
        self,
        language_resolver: TreeSitterLanguageResolverTool | None = None,
        parse_tool: TreeSitterParseTool | None = None,
        query_packs_dir: Path | None = None,
    ) -> None:
        self._language_resolver = language_resolver or TreeSitterLanguageResolverTool()
        self._parse_tool = parse_tool or TreeSitterParseTool(self._language_resolver)
        self._query_packs_dir = query_packs_dir or QUERY_PACKS_DIR
        self._query_cache: dict[str, "Query"] = {}
        self._pack_cache: dict[str, QueryPackDefinition] = {}

    def _load_query_pack(self, language_id: str) -> QueryPackDefinition | None:
        """Load query pack for a language from the .scm file."""
        if language_id in self._pack_cache:
            return self._pack_cache[language_id]

        # Look for the query file
        query_file = self._query_packs_dir / f"{language_id}.scm"
        if not query_file.exists():
            logger.debug(
                "No query pack found for language",
                language_id=language_id,
                path=str(query_file),
            )
            return None

        try:
            content = query_file.read_text(encoding="utf-8")
            queries = self._parse_query_pack(content, language_id)

            pack = QueryPackDefinition(
                pack_id=f"ts_{language_id}",
                language_id=language_id,
                queries=queries,
            )
            self._pack_cache[language_id] = pack
            logger.debug(
                "Loaded query pack",
                language_id=language_id,
                query_count=len(queries),
            )
            return pack

        except Exception as e:
            logger.error(
                "Failed to load query pack",
                language_id=language_id,
                error=str(e),
            )
            return None

    def _parse_query_pack(
        self,
        content: str,
        language_id: str,
    ) -> list[dict[str, Any]]:
        """Parse a .scm query pack file into query definitions.

        The .scm file format supports metadata comments:
        ; @rule_id: ts.public_function
        ; @facet_slot: api.public.protocols
        ; @leaf_id: 25
        ; @confidence: 0.8
        (function_definition name: (identifier) @fn_name)
        """
        queries: list[dict[str, Any]] = []
        current_metadata: dict[str, str] = {}
        current_query_lines: list[str] = []
        paren_depth = 0  # Track parentheses balance

        def flush_query() -> None:
            nonlocal current_metadata
            if current_query_lines:
                query_text = "\n".join(current_query_lines).strip()
                if query_text:
                    queries.append({
                        "query_text": query_text,
                        "rule_id": current_metadata.get("rule_id", f"ts.{language_id}.unnamed"),
                        "facet_slot": current_metadata.get("facet_slot", "unknown"),
                        "leaf_id": current_metadata.get("leaf_id", "0"),
                        "confidence": float(current_metadata.get("confidence", "0.7")),
                        "capture_name": current_metadata.get("capture", None),
                    })
                    current_metadata = {}

        for line in content.split("\n"):
            stripped = line.strip()

            # Check for metadata comments
            if stripped.startswith("; @"):
                # Flush previous query if we have one and paren_depth is 0
                if current_query_lines and paren_depth == 0:
                    flush_query()
                    current_query_lines = []

                # Parse metadata
                try:
                    key_value = stripped[3:]  # Remove "; @"
                    if ":" in key_value:
                        key, value = key_value.split(":", 1)
                        current_metadata[key.strip()] = value.strip()
                except Exception:
                    pass
            elif stripped.startswith(";"):
                # Regular comment, skip
                continue
            elif stripped:
                # Query content - track parentheses to know when query ends
                # Count parens (ignoring those inside strings/predicates for simplicity)
                paren_depth += stripped.count("(") - stripped.count(")")
                current_query_lines.append(line)

        # Flush last query
        flush_query()

        return queries

    def _compile_query(
        self,
        query_text: str,
        language_id: str,
    ) -> "Query | None":
        """Compile a tree-sitter query."""
        cache_key = f"{language_id}:{hash(query_text)}"
        if cache_key in self._query_cache:
            return self._query_cache[cache_key]

        try:
            from tree_sitter import Query
            import tree_sitter_language_pack

            language = tree_sitter_language_pack.get_language(language_id)
            query = Query(language, query_text)
            self._query_cache[cache_key] = query
            return query

        except Exception as e:
            logger.warning(
                "Failed to compile query",
                language_id=language_id,
                error=str(e),
            )
            return None

    def run(self, inp: TreeSitterQueryInput) -> TreeSitterQueryOutput:
        """Run queries against a file and return matches.

        Args:
            inp: Input containing file context and build context

        Returns:
            TreeSitterQueryOutput with detected matches
        """
        file_ctx = inp.file
        matches: list[ToolMatch] = []
        queries_executed = 0

        try:
            # Resolve the language
            lang_info = self._language_resolver.resolve(file_ctx)
        except ValueError as e:
            # Log at info level - this is expected for known unparseable files
            if self._language_resolver.is_known_unparseable(file_ctx.file_path):
                logger.debug(
                    "Skipping query for known unparseable file type",
                    file_path=file_ctx.file_path,
                )
            else:
                logger.info(
                    "Language resolution failed for query",
                    file_path=file_ctx.file_path,
                    error=str(e),
                )
            return TreeSitterQueryOutput(matches=[], queries_executed=0)

        # Load the query pack
        pack = self._load_query_pack(lang_info.id)
        if not pack:
            logger.debug(
                "No query pack available",
                file_path=file_ctx.file_path,
                language_id=lang_info.id,
            )
            return TreeSitterQueryOutput(matches=[], queries_executed=0)

        # Parse the file
        tree = self._parse_tool.parse_content(file_ctx.content_utf8, lang_info.id)
        content_bytes = file_ctx.content_utf8.encode("utf-8")

        # Run each query
        for query_def in pack.queries:
            # Filter by query_pack_ids if specified
            if inp.query_pack_ids and query_def["rule_id"] not in inp.query_pack_ids:
                continue

            query = self._compile_query(query_def["query_text"], lang_info.id)
            if not query:
                continue

            queries_executed += 1

            try:
                # tree-sitter uses QueryCursor to execute queries
                # QueryCursor.matches() returns (pattern_idx, captures_dict)
                from tree_sitter import QueryCursor
                cursor = QueryCursor(query)
                query_matches = cursor.matches(tree.root_node)

                for _pattern_idx, captures_dict in query_matches:
                    # Process each capture in the match
                    for capture_name, nodes in captures_dict.items():
                        # Skip if we're looking for a specific capture and this isn't it
                        if query_def.get("capture_name") and capture_name != query_def["capture_name"]:
                            continue

                        for node in nodes:
                            # Extract the matched value
                            value = content_bytes[node.start_byte : node.end_byte].decode(
                                "utf-8", errors="replace"
                            )

                            # Create evidence span
                            span = EvidenceSpan(
                                file_path=file_ctx.file_path,
                                start_byte=node.start_byte,
                                end_byte=node.end_byte,
                                start_line=node.start_point[0] + 1,  # 1-indexed
                                end_line=node.end_point[0] + 1,
                                source="tree_sitter",
                            )

                            # Create match
                            match = ToolMatch(
                                rule_id=query_def["rule_id"],
                                facet_slot=query_def["facet_slot"],
                                leaf_id=query_def["leaf_id"],
                                value=value,
                                confidence=query_def["confidence"],
                                spans=[span],
                            )
                            matches.append(match)

            except Exception as e:
                logger.warning(
                    "Query execution failed",
                    rule_id=query_def["rule_id"],
                    error=str(e),
                )

        logger.info(
            "Query execution complete",
            file_path=file_ctx.file_path,
            language=lang_info.name,
            queries_executed=queries_executed,
            matches_found=len(matches),
        )

        return TreeSitterQueryOutput(
            matches=matches,
            queries_executed=queries_executed,
        )
