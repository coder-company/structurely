import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("compare_benchmarks.py")
SPEC = importlib.util.spec_from_file_location("compare_benchmarks", MODULE_PATH)
assert SPEC and SPEC.loader
compare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(compare)


class CompareBenchmarksTests(unittest.TestCase):
    def test_ratio_reports_baseline_over_candidate(self):
        self.assertEqual(compare.ratio(200.0, 100.0), 2.0)
        self.assertIsNone(compare.ratio(200.0, 0.0))

    def test_load_requires_an_object(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.json"
            path.write_text("[]", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "JSON object"):
                compare.load(path)

    def test_positive_number_rejects_negative_and_boolean_values(self):
        for value in (-1, True):
            with self.subTest(value=value):
                with self.assertRaisesRegex(ValueError, "non-negative number"):
                    compare.positive_number({"metric": value}, "metric")


if __name__ == "__main__":
    unittest.main()
