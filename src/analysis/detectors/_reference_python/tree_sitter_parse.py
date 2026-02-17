"""Tree-sitter parse tool.

Parses source code files using tree-sitter and returns a compact
representation of the syntax tree.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

from actual_logger import create_logger

from ...schemas.tools_io import TreeSitterParseInput, TreeSitterParseOutput
from .language_resolver import TreeSitterLanguageResolverTool

if TYPE_CHECKING:
    from tree_sitter import Node, Parser, Tree

logger = create_logger(service="adr-analysis-agent", component="tree-sitter-parse")


class TreeSitterParseTool:
    """Parses source code using tree-sitter.

    This tool parses source code files and returns a compact representation
    of the syntax tree with stable spans for evidence tracking.
    """

    name = "tree_sitter_parse"

    def __init__(
        self,
        language_resolver: TreeSitterLanguageResolverTool | None = None,
    ) -> None:
        self._language_resolver = language_resolver or TreeSitterLanguageResolverTool()
        self._parser_cache: dict[str, "Parser"] = {}

    def _get_language_module(self):
        """Get the tree-sitter language module."""
        import tree_sitter_language_pack
        return tree_sitter_language_pack

    def _get_parser(self, lang_id: str) -> "Parser":
        """Get or create a parser for a language."""
        if lang_id not in self._parser_cache:
            from tree_sitter import Parser

            parser = Parser()
            lang_module = self._get_language_module()
            parser.language = lang_module.get_language(lang_id)

            self._parser_cache[lang_id] = parser
        return self._parser_cache[lang_id]

    def _build_node_index(
        self,
        tree: "Tree",
        content_bytes: bytes,
        max_depth: int = 3,
    ) -> dict[str, dict[str, Any]]:
        """Build a compact index of key nodes in the tree.

        Only includes top-level definitions and important structural nodes,
        not the full tree.
        """
        index: dict[str, dict[str, Any]] = {}

        def add_node(node: "Node", path: str) -> None:
            node_info = {
                "type": node.type,
                "start_byte": node.start_byte,
                "end_byte": node.end_byte,
                "start_point": {"row": node.start_point[0], "column": node.start_point[1]},
                "end_point": {"row": node.end_point[0], "column": node.end_point[1]},
            }
            # Add name if this is a named definition
            if node.type in (
                "function_definition",
                "function_declaration",
                "class_definition",
                "class_declaration",
                "method_definition",
                "method_declaration",
                "interface_declaration",
                "type_alias_declaration",
                "variable_declaration",
                "lexical_declaration",
            ):
                name_node = node.child_by_field_name("name")
                if name_node:
                    node_info["name"] = content_bytes[
                        name_node.start_byte : name_node.end_byte
                    ].decode("utf-8", errors="replace")

            index[path] = node_info

        def walk(node: "Node", path: str, depth: int) -> None:
            if depth > max_depth:
                return

            add_node(node, path)

            # Only recurse into important child nodes
            child_idx = 0
            for child in node.children:
                if child.is_named:
                    child_path = f"{path}/{child.type}[{child_idx}]"
                    walk(child, child_path, depth + 1)
                    child_idx += 1

        walk(tree.root_node, "root", 0)
        return index

    def run(self, inp: TreeSitterParseInput) -> TreeSitterParseOutput:
        """Parse a file and return tree info.

        Args:
            inp: Input containing the file context

        Returns:
            TreeSitterParseOutput with language info and node index
        """
        file_ctx = inp.file
        parse_error: str | None = None

        try:
            # Resolve the language
            lang_info = self._language_resolver.resolve(file_ctx)
        except ValueError as e:
            # Log at info level for known file types, warning for unknown
            if self._language_resolver.is_known_unparseable(file_ctx.file_path):
                logger.info(
                    "Skipping known unparseable file type",
                    file_path=file_ctx.file_path,
                )
            else:
                logger.info(
                    "Language resolution failed for unknown file type",
                    file_path=file_ctx.file_path,
                    error=str(e),
                )
            return TreeSitterParseOutput(
                language="unknown",
                tree_sitter_language_id="unknown",
                node_index={},
                parse_error=str(e),
            )

        try:
            # Get or create the parser
            parser = self._get_parser(lang_info.id)

            # Parse the content
            content_bytes = file_ctx.content_utf8.encode("utf-8")
            tree = parser.parse(content_bytes)

            # Check for parse errors
            if tree.root_node.has_error:
                parse_error = "Tree contains syntax errors"

            # Build compact node index
            node_index = self._build_node_index(tree, content_bytes)

            logger.debug(
                "Parsed file",
                file_path=file_ctx.file_path,
                language=lang_info.name,
                node_count=len(node_index),
                has_errors=tree.root_node.has_error,
            )

            return TreeSitterParseOutput(
                language=lang_info.name,
                tree_sitter_language_id=lang_info.id,
                node_index=node_index,
                parse_error=parse_error,
            )

        except Exception as e:
            logger.error(
                "Parse failed",
                file_path=file_ctx.file_path,
                error=str(e),
            )
            return TreeSitterParseOutput(
                language=lang_info.name,
                tree_sitter_language_id=lang_info.id,
                node_index={},
                parse_error=str(e),
            )

    def parse_content(
        self,
        content: str,
        language_id: str,
    ) -> "Tree":
        """Parse content directly without FileContext.

        This is a lower-level method for direct tree access.

        Args:
            content: Source code content
            language_id: Tree-sitter language ID

        Returns:
            Tree-sitter Tree object
        """
        parser = self._get_parser(language_id)
        return parser.parse(content.encode("utf-8"))
