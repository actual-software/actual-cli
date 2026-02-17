"""Language normalization helpers."""

from __future__ import annotations


def normalize_language(value: str | None) -> str | None:
    """Normalize language hints into canonical identifiers.

    Args:
        value: Raw language string from upstream systems

    Returns:
        Normalized language identifier or None
    """
    if value is None:
        return None

    lang = value.strip().lower()
    if not lang:
        return None

    # Normalize common aliases
    if lang in {"c#", "c_sharp", "c-sharp", "cs", "csharp"}:
        return "csharp"
    if lang in {"js", "jsx"}:
        return "javascript"
    if lang in {"ts", "tsx"}:
        return "typescript"
    if lang in {"py"}:
        return "python"
    if lang in {"golang"}:
        return "go"
    if lang in {"c++", "cxx"}:
        return "cpp"
    return lang
