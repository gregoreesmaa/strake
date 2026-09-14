#!/usr/bin/env python3
"""Tests for wpt_ratchet.py: per-dir WPT baseline ratchets (issue #140)."""

import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "wpt_ratchet.py")
COVERED = ["css", "dom/nodes"]


def make_report(results):
    return {"run_info": {"revision": "pin123", "product": "strake"}, "results": results}


def write_json(path, payload):
    with open(path, "w", encoding="utf-8") as file:
        json.dump(payload, file)


def run_ratchet(*args):
    return subprocess.run(
        [sys.executable, SCRIPT, *args],
        capture_output=True,
        text=True,
        check=False,
    )


class RatchetTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.report_path = os.path.join(self.tmp.name, "report.json")
        self.baselines_path = os.path.join(self.tmp.name, "baselines.json")

    def tearDown(self):
        self.tmp.cleanup()

    def base_results(self):
        return [
            {"test": "css/a.html", "status": "PASS"},
            {"test": "css/b.html", "status": "FAIL"},
            {
                "test": "dom/nodes/c.html",
                "status": "OK",
                "subtests": [
                    {"name": "one", "status": "PASS"},
                    {"name": "two", "status": "FAIL"},
                ],
            },
        ]

    def write_report(self, results):
        write_json(self.report_path, make_report(results))

    def write_baselines(self, payload):
        write_json(self.baselines_path, payload)

    def test_update_writes_per_dir_baselines(self):
        self.write_report(self.base_results())
        proc = run_ratchet("update", self.report_path, self.baselines_path,
                           "--dirs", ",".join(COVERED))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        with open(self.baselines_path, encoding="utf-8") as file:
            baselines = json.load(file)
        css = baselines["dirs"]["css"]
        self.assertEqual(
            (css["tests_pass"], css["tests_total"]), (1, 2),
            "one passing test of two, no subtests",
        )
        dom = baselines["dirs"]["dom/nodes"]
        self.assertEqual(
            (dom["tests_pass"], dom["tests_total"]), (0, 1),
            "a test with a failing subtest does not count as passing",
        )
        self.assertEqual(
            (dom["subtests_pass"], dom["subtests_total"]), (1, 2),
        )
        self.assertIn("wpt_revision", baselines["generated_from"])

    def test_check_passes_when_report_meets_baselines(self):
        self.write_report(self.base_results())
        proc = run_ratchet("update", self.report_path, self.baselines_path,
                           "--dirs", ",".join(COVERED))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        proc = run_ratchet("check", self.report_path, self.baselines_path,
                           "--dirs", ",".join(COVERED))
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("css", proc.stdout)

    def test_check_fails_on_regression_past_tolerance(self):
        self.write_report(self.base_results())
        self.assertEqual(
            run_ratchet("update", self.report_path, self.baselines_path,
                        "--dirs", ",".join(COVERED)).returncode, 0,
        )
        regressed = [
            {"test": "css/a.html", "status": "FAIL"},
            {"test": "css/b.html", "status": "FAIL"},
            self.base_results()[2],
        ]
        self.write_report(regressed)
        proc = run_ratchet("check", self.report_path, self.baselines_path,
                           "--dirs", ",".join(COVERED))
        self.assertEqual(proc.returncode, 1, proc.stdout)
        self.assertIn("css", proc.stdout)
        self.assertIn("REGRESSION", proc.stdout)

    def test_check_tolerates_drops_within_tolerance(self):
        self.write_report(self.base_results())
        self.assertEqual(
            run_ratchet("update", self.report_path, self.baselines_path,
                        "--dirs", ",".join(COVERED)).returncode, 0,
        )
        # dom/nodes loses its one passing subtest: tolerated with --tolerance 1.
        regressed = [
            self.base_results()[0],
            self.base_results()[1],
            {
                "test": "dom/nodes/c.html",
                "status": "OK",
                "subtests": [
                    {"name": "one", "status": "FAIL"},
                    {"name": "two", "status": "FAIL"},
                ],
            },
        ]
        self.write_report(regressed)
        proc = run_ratchet("check", self.report_path, self.baselines_path,
                           "--dirs", ",".join(COVERED), "--tolerance", "1")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        proc = run_ratchet("check", self.report_path, self.baselines_path,
                           "--dirs", ",".join(COVERED))
        self.assertEqual(proc.returncode, 1, proc.stdout)

    def test_check_passes_on_improvement_and_suggests_raising(self):
        self.write_report(self.base_results())
        self.assertEqual(
            run_ratchet("update", self.report_path, self.baselines_path,
                        "--dirs", ",".join(COVERED)).returncode, 0,
        )
        improved = [
            {"test": "css/a.html", "status": "PASS"},
            {"test": "css/b.html", "status": "PASS"},
            self.base_results()[2],
        ]
        self.write_report(improved)
        proc = run_ratchet("check", self.report_path, self.baselines_path,
                           "--dirs", ",".join(COVERED))
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("raise", proc.stdout.lower())

    def test_uncovered_dirs_do_not_fail_the_check(self):
        self.write_report(
            self.base_results()
            + [{"test": "weird-dir/z.html", "status": "FAIL"}]
        )
        self.write_baselines(
            {"generated_from": {}, "dirs": {
                "css": {"tests_pass": 1, "tests_total": 2,
                        "subtests_pass": 0, "subtests_total": 0},
                "dom/nodes": {"tests_pass": 0, "tests_total": 1,
                              "subtests_pass": 1, "subtests_total": 2},
            }}
        )
        proc = run_ratchet("check", self.report_path, self.baselines_path,
                           "--dirs", ",".join(COVERED))
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)


if __name__ == "__main__":
    unittest.main()
