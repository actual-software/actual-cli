#!/usr/bin/env python3
"""Update Homebrew formula with new version and SHA256 checksums."""

import argparse
import re
import sys
from pathlib import Path


def update_formula(
    template_path: str,
    output_path: str,
    version: str,
    sha_darwin_arm64: str,
    sha_darwin_x64: str,
    sha_linux_x64: str,
    sha_linux_arm64: str,
) -> None:
    content = Path(template_path).read_text()

    # Update version
    content = re.sub(
        r'version "[^"]*"',
        f'version "{version}"',
        content,
    )

    # Update SHA256 placeholders
    replacements = {
        "PLACEHOLDER_SHA256_DARWIN_ARM64": sha_darwin_arm64,
        "PLACEHOLDER_SHA256_DARWIN_X64": sha_darwin_x64,
        "PLACEHOLDER_SHA256_LINUX_X64": sha_linux_x64,
        "PLACEHOLDER_SHA256_LINUX_ARM64": sha_linux_arm64,
    }
    for placeholder, sha in replacements.items():
        if sha:
            content = content.replace(placeholder, sha)

    Path(output_path).parent.mkdir(parents=True, exist_ok=True)
    Path(output_path).write_text(content)
    print(f"Updated formula written to {output_path}")
    print(f"  version: {version}")
    print(f"  darwin-arm64 sha256: {sha_darwin_arm64}")
    print(f"  darwin-x64   sha256: {sha_darwin_x64}")
    print(f"  linux-x64    sha256: {sha_linux_x64}")
    print(f"  linux-arm64  sha256: {sha_linux_arm64}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("template", help="Path to the formula template file")
    parser.add_argument("output", help="Path to write the updated formula")
    parser.add_argument("--version", required=True, help="Release version (e.g. 1.2.3)")
    parser.add_argument(
        "--sha-darwin-arm64", default="", help="SHA256 for darwin-arm64 tarball"
    )
    parser.add_argument(
        "--sha-darwin-x64", default="", help="SHA256 for darwin-x64 tarball"
    )
    parser.add_argument(
        "--sha-linux-x64", default="", help="SHA256 for linux-x64 tarball"
    )
    parser.add_argument(
        "--sha-linux-arm64", default="", help="SHA256 for linux-arm64 tarball"
    )
    args = parser.parse_args()

    update_formula(
        args.template,
        args.output,
        args.version,
        args.sha_darwin_arm64,
        args.sha_darwin_x64,
        args.sha_linux_x64,
        args.sha_linux_arm64,
    )


if __name__ == "__main__":
    main()
