import unittest

try:
    from scripts.search_quality import evaluate, ranked_files, validate_manifest
except ModuleNotFoundError:  # Direct execution adds scripts/ rather than the repo root.
    from search_quality import evaluate, ranked_files, validate_manifest


class SearchQualityTests(unittest.TestCase):
    def test_ranked_files_preserve_retrieval_order_and_deduplicate(self):
        report = {
            "symbol_findings": [
                {"symbol": {"file": "src/engine.rs"}},
                {"symbol": {"file": "src/store.rs"}},
            ],
            "content_findings": [
                {"path": "src/store.rs"},
                {"path": "README.md"},
            ],
        }
        self.assertEqual(
            ranked_files(report),
            ["src/engine.rs", "src/store.rs", "README.md"],
        )

    def test_evaluation_enforces_the_maximum_rank(self):
        query = {
            "query": "atomic publication",
            "expected": "src/atomic_file.rs",
            "maximumRank": 2,
            "category": "storage",
        }
        self.assertTrue(evaluate(query, ["src/store.rs", "src/atomic_file.rs"])["passed"])
        self.assertFalse(evaluate(query, ["src/store.rs"])["passed"])

    def test_manifest_rejects_duplicate_queries_and_invalid_ranks(self):
        query = {
            "query": "same",
            "expected": "src/main.rs",
            "maximumRank": 1,
            "category": "cli",
        }
        with self.assertRaisesRegex(ValueError, "duplicate"):
            validate_manifest({"version": 1, "queries": [query, query]})
        with self.assertRaisesRegex(ValueError, "positive"):
            validate_manifest(
                {"version": 1, "queries": [{**query, "maximumRank": 0}]}
            )


if __name__ == "__main__":
    unittest.main()
