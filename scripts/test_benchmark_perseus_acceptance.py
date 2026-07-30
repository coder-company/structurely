#!/usr/bin/env python3

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("benchmark_perseus_acceptance.py")
SPEC = importlib.util.spec_from_file_location(
    "benchmark_perseus_acceptance", MODULE_PATH
)
assert SPEC and SPEC.loader
benchmark = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(benchmark)


class PerseusAcceptanceTests(unittest.TestCase):
    def test_ranked_files_preserve_symbol_then_content_retrieval_order(self) -> None:
        report = {
            "files": ["README.md", "src/a.rs", "src/b.rs"],
            "symbol_findings": [
                {"symbol": {"file": "src/b.rs"}},
                {"symbol": {"file": "src/a.rs"}},
                {"symbol": {"file": "src/b.rs"}},
            ],
            "content_findings": [
                {"path": "README.md"},
                {"path": "src/a.rs"},
            ],
        }
        self.assertEqual(
            benchmark.ranked_files(report), ["src/b.rs", "src/a.rs", "README.md"]
        )

    def test_workflow_coverage_requires_every_named_capability(self) -> None:
        complete = (
            "research session recap impact trace memory workspace"
        )
        self.assertTrue(all(benchmark.workflow_coverage(complete).values()))
        self.assertFalse(
            benchmark.workflow_coverage("research impact")["durable_memory"]
        )

    def test_rank_is_one_based_and_missing_is_none(self) -> None:
        self.assertEqual(benchmark.rank("a.rs", ["a.rs", "b.rs"]), 1)
        self.assertIsNone(benchmark.rank("missing.rs", ["a.rs", "b.rs"]))

    def test_relevance_gates_require_strict_wins(self) -> None:
        baseline = {
            "rank_one": {"perseus": 3},
            "top_ten_expected_file_recall": {"perseus": 4},
        }
        self.assertEqual(
            benchmark.relevance_gates(3, 5, baseline),
            {"rank_one_better": False, "top_ten_better": True},
        )
        self.assertEqual(
            benchmark.relevance_gates(4, 5, baseline),
            {"rank_one_better": True, "top_ten_better": True},
        )


if __name__ == "__main__":
    unittest.main()
