#!/usr/bin/env python3
"""Per-directory WPT baseline ratchets (issue #140).

The WPT workflow renders a report-only main-vs-PR diff; nothing enforces
the "never regress, raise on improvement" rule. This script is the
enforcer:

- `update <report.json> <baselines.json>`: record per-dir pass/total
  counts from a green main report (`wpt/baselines.json` is checked in).
- `check <report.json> <baselines.json>`: exit 1 when any covered dir's
  passing subtests drop past `--tolerance` (default 0). Improvements never
  fail; they print a raise-the-baselines hint instead.

A test counts as passing when its status is PASS/OK and every subtest is
PASS/OK (matching `wpt_diff_to_pr.py`'s passing set). Results outside the
covered dirs are ignored. A covered dir with no results in the report
fails when its baseline expected passes (a silent runner misconfiguration
reads as a total regression, which is what it is).
"""

import argparse
import datetime
import json
import sys

PASSING_STATUSES = {"PASS", "OK"}
DEFAULT_DIRS = ["css", "svg", "dom/nodes", "dom/events", "html/webappapis", "fetch"]


def load_report(path):
    with open(path, encoding="utf-8") as file:
        return json.load(file)


def test_passes(result):
    if result.get("status") not in PASSING_STATUSES:
        return False
    return all(
        subtest.get("status") in PASSING_STATUSES
        for subtest in result.get("subtests") or []
    )


def subtest_counts(result):
    subtests = result.get("subtests") or []
    passing = sum(1 for sub in subtests if sub.get("status") in PASSING_STATUSES)
    return passing, len(subtests)


def assign_dir(test_path, covered):
    """Longest covered-dir prefix of a report test path, else None."""
    best = None
    for directory in covered:
        if test_path == directory or test_path.startswith(directory + "/"):
            if best is None or len(directory) > len(best):
                best = directory
    return best


def summarize(report, covered):
    """Per-dir {tests_pass, tests_total, subtests_pass, subtests_total}."""
    summary = {
        directory: {
            "tests_pass": 0,
            "tests_total": 0,
            "subtests_pass": 0,
            "subtests_total": 0,
        }
        for directory in covered
    }
    for result in report.get("results", []):
        directory = assign_dir(result.get("test", ""), covered)
        if directory is None:
            continue
        entry = summary[directory]
        entry["tests_total"] += 1
        if test_passes(result):
            entry["tests_pass"] += 1
        passing, total = subtest_counts(result)
        entry["subtests_pass"] += passing
        entry["subtests_total"] += total
    return summary


def check_report(report, baselines, tolerance):
    """Compare a report against baselines.

    Returns (failures, improvements): per-dir verdict rows. A row fails
    when passing tests or passing subtests drop more than `tolerance`
    below baseline — both signals matter because reftests carry no
    subtests while testharness tests live in them.
    """
    summary = summarize(report, baselines["dirs"].keys())
    failures, improvements = [], []
    for directory, baseline in baselines["dirs"].items():
        current = summary.get(directory, {
            "tests_pass": 0, "tests_total": 0,
            "subtests_pass": 0, "subtests_total": 0,
        })
        tests_delta = current["tests_pass"] - baseline["tests_pass"]
        subtests_delta = current["subtests_pass"] - baseline["subtests_pass"]
        row = {
            "dir": directory,
            "tests": f"{current['tests_pass']}/{current['tests_total']}",
            "subtests": f"{current['subtests_pass']}/{current['subtests_total']}",
            "baseline_tests": baseline["tests_pass"],
            "baseline_subtests": baseline["subtests_pass"],
            "delta": subtests_delta,
        }
        if tests_delta < -tolerance or subtests_delta < -tolerance:
            row["verdict"] = "REGRESSION"
            failures.append(row)
        elif tests_delta > tolerance or subtests_delta > tolerance:
            row["verdict"] = "IMPROVED (raise baselines)"
            improvements.append(row)
        else:
            row["verdict"] = "ok"
    return failures, improvements


def render_table(failures, improvements, baselines, report):
    summary = summarize(report, baselines["dirs"].keys())
    lines = ["| dir | tests pass/total (Δ) | subtests pass/total (Δ) | verdict |",
             "| --- | --- | --- | --- |"]
    for directory in baselines["dirs"]:
        current = summary.get(directory, {
            "tests_pass": 0, "tests_total": 0,
            "subtests_pass": 0, "subtests_total": 0,
        })
        baseline = baselines["dirs"][directory]
        tests_delta = current["tests_pass"] - baseline["tests_pass"]
        subtests_delta = current["subtests_pass"] - baseline["subtests_pass"]
        verdict = next(
            (row["verdict"] for row in failures + improvements
             if row["dir"] == directory),
            "ok",
        )
        lines.append(
            f"| {directory} | {current['tests_pass']}/{current['tests_total']} "
            f"({tests_delta:+}) "
            f"| {current['subtests_pass']}/{current['subtests_total']} "
            f"({subtests_delta:+}) | {verdict} |"
        )
    return "\n".join(lines)


def cmd_update(args):
    report = load_report(args.report)
    summary = summarize(report, args.dirs)
    baselines = {
        "generated_from": {
            "wpt_revision": (report.get("run_info") or {}).get("revision"),
            "generated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "note": "green main report; regenerate with wpt_ratchet.py update",
        },
        "dirs": summary,
    }
    with open(args.baselines, "w", encoding="utf-8") as file:
        json.dump(baselines, file, indent=2, sort_keys=True)
        file.write("\n")
    print(f"Wrote baselines for {len(summary)} dirs to {args.baselines}")
    return 0


def cmd_check(args):
    report = load_report(args.report)
    with open(args.baselines, encoding="utf-8") as file:
        baselines = json.load(file)
    failures, improvements = check_report(report, baselines, args.tolerance)
    print(render_table(failures, improvements, baselines, report))
    if improvements and not failures:
        print("Subtests improved: raise the baselines with "
              "`wpt_ratchet.py update` on a green main report.")
    if failures:
        print(f"{len(failures)} dir(s) regressed past tolerance "
              f"{args.tolerance}; see REGRESSION rows above.")
        return 1
    print("No regressions past tolerance.")
    return 0


def parse_dirs(value):
    return [part for part in value.split(",") if part]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    update = sub.add_parser("update", help="record baselines from a report")
    update.add_argument("report")
    update.add_argument("baselines")
    update.add_argument("--dirs", default=",".join(DEFAULT_DIRS))

    check = sub.add_parser("check", help="enforce baselines on a report")
    check.add_argument("report")
    check.add_argument("baselines")
    check.add_argument("--dirs", default=",".join(DEFAULT_DIRS))
    check.add_argument("--tolerance", type=int, default=0)

    args = parser.parse_args(argv)
    args.dirs = parse_dirs(args.dirs)
    if args.command == "update":
        return cmd_update(args)
    return cmd_check(args)


if __name__ == "__main__":
    sys.exit(main())
