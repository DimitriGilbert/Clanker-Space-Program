#!/usr/bin/env python3
"""PLAN §8.2 Coverage gate — compare a `cargo llvm-cov --json` report
against ci/coverage-floors.json.

The llvm-cov JSON is file-level (`data[].files[].filename` +
`summary.lines`), so per-crate coverage is aggregated by the
`crates/<name>/` path prefix — files under `target/` or outside the
workspace never carry a floor anyway.

Two checks per floored crate (line coverage, weighted by line count):

- floor: measured must be >= the crate's `floor` (the §8.2 spec floor,
  or the last recorded measurement if higher — the file bumps upward
  only);
- ratchet: measured must not be more than `ratchet_pp` below the
  `recorded` value — §8.2's "no decrease >0.5 pp".

A floored crate absent from the report is a harness failure, not a
pass. Unfloored crates are reported for visibility only.
"""

import json
import sys
from pathlib import Path


def crate_of(filename: str) -> str | None:
    parts = Path(filename).parts
    if "crates" in parts:
        i = parts.index("crates")
        if i + 1 < len(parts):
            return parts[i + 1]
    return None


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: check_coverage.py <llvm-cov.json> <coverage-floors.json>",
              file=sys.stderr)
        return 2

    with open(sys.argv[1], encoding="utf-8") as f:
        report = json.load(f)
    with open(sys.argv[2], encoding="utf-8") as f:
        spec = json.load(f)

    # Aggregate file rows into per-crate line counts.
    measured: dict[str, tuple[int, int]] = {}  # crate -> (covered, count)
    for data in report.get("data", []):
        for file_ in data.get("files", []):
            name = crate_of(file_.get("filename", ""))
            if name is None:
                continue
            lines = file_.get("summary", {}).get("lines", {})
            covered, count = lines.get("covered", 0), lines.get("count", 0)
            prev = measured.get(name, (0, 0))
            measured[name] = (prev[0] + covered, prev[1] + count)

    if not measured:
        print("check_coverage: no coverage data in report — harness failure",
              file=sys.stderr)
        return 2

    ratchet_pp = float(spec.get("ratchet_pp", 0.5))
    failed = False
    for name, row in sorted(spec["crates"].items()):
        floor = float(row["floor"])
        recorded = row.get("recorded")
        cov = measured.get(name)
        if cov is None or cov[1] == 0:
            print(f"FAIL {name}: floored crate absent from coverage report")
            failed = True
            continue
        got = 100.0 * cov[0] / cov[1]
        status = []
        if got < floor:
            status.append(f"below floor {floor:.2f}")
            failed = True
        if recorded is not None and got < float(recorded) - ratchet_pp:
            status.append(
                f"regressed {got:.2f} vs recorded {float(recorded):.2f} "
                f"(ratchet {ratchet_pp} pp)"
            )
            failed = True
        verdict = "FAIL" if status else "ok"
        note = f" — {'; '.join(status)}" if status else \
            (f" — above recorded {float(recorded):.2f}; bump the floor file"
             if recorded is not None and got > float(recorded) + ratchet_pp
             else "")
        print(f"{verdict} {name}: {got:.2f}% lines (floor {floor:.2f}){note}")

    for name, (covered, count) in sorted(measured.items()):
        if name not in spec["crates"] and count:
            print(f"info {name}: {100.0 * covered / count:.2f}% lines "
                  "(no floor — §8.2 unfloored)")

    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
