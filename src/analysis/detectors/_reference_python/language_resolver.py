"""Tree-sitter language resolver tool.

Resolves file extensions and language hints to tree-sitter language grammars.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from actual_logger import create_logger

if TYPE_CHECKING:
    from tree_sitter import Language

    from ...schemas.tools_io import BuildContext, FileContext

logger = create_logger(service="adr-analysis-agent", component="tree-sitter-lang-resolver")

# Known file types that we recognize but cannot parse with tree-sitter
# These are logged at info level instead of warning when skipped
KNOWN_UNPARSEABLE_EXTENSIONS: set[str] = {
    ".baml",        # BAML config files
    ".toml",        # TOML config files
    ".ini",         # INI config files
    ".env",         # Environment files
    ".lock",        # Lock files (package-lock.json handled by .json)
    ".gitignore",   # Git ignore files
    ".dockerignore",# Docker ignore files
    ".editorconfig",# Editor config
    ".prettierrc",  # Prettier config
    ".eslintrc",    # ESLint config (when not .json/.js)
    ".nvmrc",       # Node version manager
    ".tf",          # Terraform configuration files
    ".tfvars",      # Terraform variable files
    ".hcl",         # HashiCorp Configuration Language
    ".tfstate",     # Terraform state files
}

# Mapping from file extensions to tree-sitter language IDs
# Uses tree-sitter-languages package language names
EXTENSION_TO_LANGUAGE: dict[str, str] = {
    # JavaScript/TypeScript
    ".js": "javascript",
    ".mjs": "javascript",
    ".cjs": "javascript",
    ".jsx": "javascript",
    ".ts": "typescript",
    ".mts": "typescript",
    ".cts": "typescript",
    ".tsx": "tsx",
    # Python
    ".py": "python",
    ".pyi": "python",
    # Go
    ".go": "go",
    # Java
    ".java": "java",
    # Kotlin
    ".kt": "kotlin",
    ".kts": "kotlin",
    # Rust
    ".rs": "rust",
    # C/C++
    ".c": "c",
    ".h": "c",
    ".cc": "cpp",
    ".cpp": "cpp",
    ".cxx": "cpp",
    ".hpp": "cpp",
    ".hh": "cpp",
    ".hxx": "cpp",
    # C#
    ".cs": "csharp",
    # Ruby
    ".rb": "ruby",
    # PHP
    ".php": "php",
    # Swift
    ".swift": "swift",
    # SQL
    ".sql": "sql",
    # Shell
    ".sh": "bash",
    ".bash": "bash",
    # YAML/JSON (for config detection)
    ".yaml": "yaml",
    ".yml": "yaml",
    ".json": "json",
    # HTML/CSS
    ".html": "html",
    ".htm": "html",
    ".css": "css",
    # Markdown
    ".md": "markdown",
}

# Mapping from language hints to tree-sitter language IDs
LANGUAGE_HINT_TO_ID: dict[str, str] = {
    "javascript": "javascript",
    "js": "javascript",
    "typescript": "typescript",
    "ts": "typescript",
    "tsx": "tsx",
    "jsx": "javascript",
    "python": "python",
    "py": "python",
    "go": "go",
    "golang": "go",
    "java": "java",
    "kotlin": "kotlin",
    "kt": "kotlin",
    "rust": "rust",
    "rs": "rust",
    "c": "c",
    "cpp": "cpp",
    "c++": "cpp",
    "cxx": "cpp",
    "csharp": "csharp",
    "c#": "csharp",
    "cs": "csharp",
    "ruby": "ruby",
    "rb": "ruby",
    "php": "php",
    "swift": "swift",
    "sql": "sql",
    "bash": "bash",
    "shell": "bash",
    "sh": "bash",
    "yaml": "yaml",
    "yml": "yaml",
    "json": "json",
    "html": "html",
    "css": "css",
    "markdown": "markdown",
    "md": "markdown",
}

# Human-readable language names
LANGUAGE_ID_TO_NAME: dict[str, str] = {
    "javascript": "JavaScript",
    "typescript": "TypeScript",
    "tsx": "TypeScript (TSX)",
    "python": "Python",
    "go": "Go",
    "java": "Java",
    "kotlin": "Kotlin",
    "rust": "Rust",
    "c": "C",
    "cpp": "C++",
    "csharp": "C#",
    "ruby": "Ruby",
    "php": "PHP",
    "swift": "Swift",
    "sql": "SQL",
    "bash": "Bash",
    "yaml": "YAML",
    "json": "JSON",
    "html": "HTML",
    "css": "CSS",
    "markdown": "Markdown",
}


@dataclass
class LanguageInfo:
    """Information about a resolved tree-sitter language."""

    id: str  # tree-sitter language ID (e.g., "typescript")
    name: str  # Human-readable name (e.g., "TypeScript")
    language_obj: "Language"  # tree-sitter Language object


class TreeSitterLanguageResolverTool:
    """Resolves file paths and language hints to tree-sitter languages.

    This tool determines which tree-sitter grammar to use for parsing
    a file based on its extension and optional language hints.
    """

    name = "tree_sitter_language_resolver"

    def __init__(self) -> None:
        self._language_cache: dict[str, "Language"] = {}

    def _get_language(self, lang_id: str) -> "Language":
        """Get or load a tree-sitter Language object."""
        if lang_id not in self._language_cache:
            try:
                import tree_sitter_language_pack
                self._language_cache[lang_id] = tree_sitter_language_pack.get_language(
                    lang_id
                )
            except Exception as e:
                logger.warning(
                    "Failed to load tree-sitter language",
                    language_id=lang_id,
                    error=str(e),
                )
                raise ValueError(f"Unsupported tree-sitter language: {lang_id}") from e
        return self._language_cache[lang_id]

    def resolve(
        self,
        file: "FileContext",
        build_context: "BuildContext | None" = None,
    ) -> LanguageInfo:
        """Resolve the tree-sitter language for a file.

        Args:
            file: File context with path and optional language hint
            build_context: Optional build context for additional hints

        Returns:
            LanguageInfo with the resolved language

        Raises:
            ValueError: If the language cannot be determined or is unsupported
        """
        lang_id: str | None = None
        ext = Path(file.file_path).suffix.lower()

        # 1. For JSX/TSX files, always use extension-based grammar
        # because they require specific grammars for JSX syntax
        if ext in (".tsx", ".jsx"):
            lang_id = EXTENSION_TO_LANGUAGE.get(ext)
        # 2. Try language hint
        elif file.language_hint:
            hint_lower = file.language_hint.lower()
            if hint_lower in LANGUAGE_HINT_TO_ID:
                lang_id = LANGUAGE_HINT_TO_ID[hint_lower]

        # 3. Fall back to file extension
        if not lang_id:
            if ext in EXTENSION_TO_LANGUAGE:
                lang_id = EXTENSION_TO_LANGUAGE[ext]

        # 3. Check if we found a language
        if not lang_id:
            raise ValueError(
                f"Cannot determine language for file: {file.file_path} "
                f"(hint: {file.language_hint})"
            )

        # 4. Get the Language object
        language_obj = self._get_language(lang_id)
        name = LANGUAGE_ID_TO_NAME.get(lang_id, lang_id.title())

        logger.debug(
            "Resolved language",
            file_path=file.file_path,
            language_id=lang_id,
            language_name=name,
        )

        return LanguageInfo(
            id=lang_id,
            name=name,
            language_obj=language_obj,
        )

    def is_supported(self, file_path: str, language_hint: str | None = None) -> bool:
        """Check if a file's language is supported.

        Args:
            file_path: Path to the file
            language_hint: Optional language hint

        Returns:
            True if the language is supported
        """
        if language_hint and language_hint.lower() in LANGUAGE_HINT_TO_ID:
            return True

        ext = Path(file_path).suffix.lower()
        return ext in EXTENSION_TO_LANGUAGE

    def is_known_unparseable(self, file_path: str) -> bool:
        """Check if a file is a known type that cannot be parsed.

        These are recognized file types (like .baml, .toml) that we don't
        have tree-sitter grammars for. They should be logged at info level
        rather than warning level when skipped.

        Args:
            file_path: Path to the file

        Returns:
            True if the file type is known but unparseable
        """
        ext = Path(file_path).suffix.lower()
        # Also treat files with no extension in test fixture dirs as known
        if not ext:
            path_lower = file_path.lower()
            if "__fixtures__" in path_lower or "__snapshots__" in path_lower:
                return True
        return ext in KNOWN_UNPARSEABLE_EXTENSIONS

    def list_supported_languages(self) -> list[str]:
        """List all supported language IDs."""
        return sorted(set(EXTENSION_TO_LANGUAGE.values()))
