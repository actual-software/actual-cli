"""Semgrep rule pack resolver tool.

Resolves which Semgrep rule packs to apply based on language and facet scope.
Uses ONLY custom rules from packages/custom-semgrep-architecture-rules.

IMPORTANT: No external semgrep rule packs or legacy rules are allowed.
All rules must come from the custom-semgrep-architecture-rules package.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

from actual_logger import create_logger

if TYPE_CHECKING:
    from ...schemas.tools_io import BuildContext, FileContext

logger = create_logger(service="adr-analysis-agent", component="semgrep-rule-resolver")

# Directory containing custom semgrep rules (hierarchical structure)
# Path: packages/custom-semgrep-architecture-rules/rules/
# This is the ONLY allowed source for semgrep rules - no fallbacks.
CUSTOM_RULES_PACKAGE_DIR = (
    Path(__file__).parent.parent.parent.parent.parent.parent
    / "custom-semgrep-architecture-rules"
    / "rules"
)

# All rule categories in the new package structure
RULE_CATEGORIES = [
    "auth",
    "security",
    "observability",
    "api",
    "data",
    "messaging",
    "infrastructure",
    "testing",
]

# Languages supported by the custom rules package
# All languages get all categories - language filtering happens at semgrep level
SUPPORTED_LANGUAGES = [
    "javascript",
    "typescript",
    "python",
    "go",
    "java",
    "rust",
    "csharp",
    "kotlin",
    "ruby",
    "php",
    "swift",
    "c",
    "cpp",
    "scala",
]


class SemgrepRulePackResolverTool:
    """Resolves which Semgrep rule packs to apply for a file.

    This tool determines which rule packs are applicable based on
    the file's language. Uses ONLY the custom-semgrep-architecture-rules package.

    No external semgrep rule packs or legacy rules are allowed.
    """

    name = "semgrep_rule_pack_resolver"

    def __init__(self, rule_packs_dir: Path | None = None) -> None:
        # Use only custom-semgrep-architecture-rules package - no fallbacks allowed
        if rule_packs_dir:
            self._rule_packs_dir = rule_packs_dir
        else:
            self._rule_packs_dir = CUSTOM_RULES_PACKAGE_DIR

        if not self._rule_packs_dir.exists():
            raise FileNotFoundError(
                f"Custom semgrep rules directory not found: {self._rule_packs_dir}. "
                "The custom-semgrep-architecture-rules package must be present."
            )

        self._available_rule_files: list[Path] | None = None

    def _scan_available_rules(self) -> list[Path]:
        """Scan for available rule files in the hierarchical structure.

        Scans rules/{category}/*.yml structure from custom-semgrep-architecture-rules.
        """
        if self._available_rule_files is not None:
            return self._available_rule_files

        self._available_rule_files = []
        # Hierarchical structure: rules/{category}/*.yml
        for yml_file in self._rule_packs_dir.rglob("*.yml"):
            self._available_rule_files.append(yml_file)
        for yaml_file in self._rule_packs_dir.rglob("*.yaml"):
            self._available_rule_files.append(yaml_file)

        logger.debug(
            "Scanned semgrep rule files",
            directory=str(self._rule_packs_dir),
            file_count=len(self._available_rule_files),
        )
        return self._available_rule_files

    def _normalize_language(self, lang: str) -> str:
        """Normalize language name to standard form."""
        lang = lang.lower()
        # Common aliases
        if lang in ("ts", "tsx"):
            return "typescript"
        elif lang in ("js", "jsx"):
            return "javascript"
        elif lang == "py":
            return "python"
        elif lang == "golang":
            return "go"
        elif lang in ("c++", "cxx"):
            return "cpp"
        elif lang == "cs":
            return "csharp"
        elif lang == "kt":
            return "kotlin"
        elif lang == "rb":
            return "ruby"
        return lang

    def resolve(
        self,
        file: "FileContext",
        build_context: "BuildContext | None" = None,
    ) -> list[str]:
        """Resolve applicable rule pack paths for a file.

        Args:
            file: File context with path and language hint
            build_context: Optional build context

        Returns:
            List of rule pack file paths to apply
        """
        rule_files = self._scan_available_rules()

        # Determine language
        lang = self._normalize_language(file.language_hint or "")

        # Return all rule files - semgrep filters by language based on 'languages' field
        rule_paths = [str(f) for f in rule_files]

        logger.debug(
            "Resolved rule packs",
            file_path=file.file_path,
            language=lang,
            pack_count=len(rule_paths),
        )

        return rule_paths

    def resolve_by_category(self, category: str) -> list[str]:
        """Resolve rule files for a specific category.

        Args:
            category: Category name (e.g., 'auth', 'security', 'api')

        Returns:
            List of rule file paths for that category
        """
        rule_files = self._scan_available_rules()
        category_dir = self._rule_packs_dir / category

        return [str(f) for f in rule_files if f.parent == category_dir]

    def list_available_packs(self) -> list[str]:
        """List all available rule file paths."""
        return [str(f) for f in self._scan_available_rules()]

    def list_categories(self) -> list[str]:
        """List all available rule categories."""
        categories = set()
        for rule_file in self._scan_available_rules():
            # Category is the parent directory name
            if rule_file.parent != self._rule_packs_dir:
                categories.add(rule_file.parent.name)

        return sorted(categories)
