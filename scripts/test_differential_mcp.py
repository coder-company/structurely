#!/usr/bin/env python3

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("differential_mcp.py")
SPEC = importlib.util.spec_from_file_location("differential_mcp", MODULE_PATH)
assert SPEC and SPEC.loader
differential = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(differential)


class BaselineCodeGraphCaptureTests(unittest.TestCase):
    def test_extracts_codegraph_capture_from_prior_report(self) -> None:
        capture = {"initialize": {"protocolVersion": "2025-06-18"}}
        report = {
            "compatibility": {"passed": 22, "total": 22},
            "captures": {"structurely": {}, "codegraph": capture},
        }

        self.assertEqual(differential.baseline_codegraph_capture(report), capture)

    def test_preserves_legacy_raw_capture(self) -> None:
        capture = {"initialize": {"protocolVersion": "2025-06-18"}}

        self.assertEqual(differential.baseline_codegraph_capture(capture), capture)

    def test_preserves_non_mapping_capture(self) -> None:
        capture = ["normalized", "messages"]

        self.assertEqual(differential.baseline_codegraph_capture(capture), capture)


class CapabilityProbePredicateTests(unittest.TestCase):
    def test_requires_exact_successful_empty_capability_collections(self) -> None:
        capture = {
            "initialize": {"result": {"protocolVersion": "2025-06-18"}},
            "tools": {"result": {"tools": []}},
            "resources": {"result": {"resources": []}},
            "resource-templates": {"result": {"resourceTemplates": []}},
            "prompts": {"result": {"prompts": []}},
        }

        capture.update(
            {
                label: {"result": {"content": []}}
                for label in (
                    "search-exact",
                    "search-ambiguous",
                    "callers",
                    "callees",
                    "react-rerender",
                    "jsx-child-render",
                    "interface-dispatch",
                    "arkui-event",
                    "arkui-state",
                    "ohpm-package",
                    "arkui-route",
                    "arkui-helper",
                    "impact",
                    "node-window",
                    "explore-flow",
                    "status",
                    "files",
                    "missing-required",
                    "invalid-limit",
                    "missing-symbol",
                )
            }
        )
        checks = differential.predicates(capture)
        self.assertTrue(checks["resources"])
        self.assertTrue(checks["resource-templates"])
        self.assertTrue(checks["prompts"])

        capture["resources"] = {"result": {"resources": ["unexpected"]}}
        self.assertFalse(differential.predicates(capture)["resources"])


if __name__ == "__main__":
    unittest.main()
