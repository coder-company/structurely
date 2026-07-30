#!/usr/bin/env python3
"""Focused tests for the real-repository acceptance evidence helpers."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import acceptance_repositories as acceptance


class AcceptanceEvidenceTests(unittest.TestCase):
    def test_percentile_uses_nearest_rank(self) -> None:
        samples = [8.0, 1.0, 5.0, 3.0, 2.0]
        self.assertEqual(acceptance.percentile(samples, 50), 3.0)
        self.assertEqual(acceptance.percentile(samples, 95), 8.0)

    def test_directory_bytes_ignores_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "nested").mkdir()
            (root / "graph.db").write_bytes(b"graph")
            (root / "nested" / "content.db").write_bytes(b"content")
            (root / "linked.db").symlink_to(root / "graph.db")
            self.assertEqual(acceptance.directory_bytes(root), 12)

    def test_limits_are_opt_in_and_reject_unknown_metrics(self) -> None:
        acceptance.enforce_limits(
            "fixture", {"queryP95Ms": 4.5}, {"queryP95Ms": 5}
        )
        with self.assertRaisesRegex(RuntimeError, "expected at most 4"):
            acceptance.enforce_limits(
                "fixture", {"queryP95Ms": 4.5}, {"queryP95Ms": 4}
            )
        with self.assertRaisesRegex(ValueError, "unknown performance limit"):
            acceptance.enforce_limits("fixture", {}, {"typo": 1})


if __name__ == "__main__":
    unittest.main()
