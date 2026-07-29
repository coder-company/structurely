#!/usr/bin/env python3
"""Validate local Markdown links and basic documentation structure."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
DOCS = [
    ROOT / "README.md",
    ROOT / "CONTRIBUTING.md",
    ROOT / "SECURITY.md",
    *sorted((ROOT / "docs").glob("*.md")),
]
LINK = re.compile(r"(?<!!)\[[^\]]+\]\(([^)]+)\)")
HEADING = re.compile(r"^(#{1,6})\s+\S")


def main() -> int:
    errors: list[str] = []
    for document in DOCS:
        text = document.read_text(encoding="utf-8")
        headings = [
            (line_number, len(match.group(1)))
            for line_number, line in enumerate(text.splitlines(), 1)
            if (match := HEADING.match(line))
        ]
        if not headings or headings[0][1] != 1:
            errors.append(f"{document.relative_to(ROOT)}: start with one H1")
        for (previous_line, previous), (line_number, current) in zip(
            headings, headings[1:]
        ):
            if current > previous + 1:
                errors.append(
                    f"{document.relative_to(ROOT)}:{line_number}: "
                    f"heading jumps from H{previous} at line {previous_line} to H{current}"
                )

        for match in LINK.finditer(text):
            raw_target = match.group(1).strip()
            target = raw_target.split(maxsplit=1)[0].strip("<>")
            if (
                not target
                or target.startswith(("#", "http://", "https://", "mailto:"))
            ):
                continue
            path_text = unquote(target.split("#", 1)[0])
            resolved = (document.parent / path_text).resolve()
            if not resolved.is_relative_to(ROOT) or not resolved.exists():
                line_number = text.count("\n", 0, match.start()) + 1
                errors.append(
                    f"{document.relative_to(ROOT)}:{line_number}: "
                    f"missing local link target {target}"
                )

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Documentation checks passed for {len(DOCS)} files.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
