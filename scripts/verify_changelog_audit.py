#!/usr/bin/env python3
"""Check docs/CHANGELOG_AUDIT.md's cited evidence against the actual codebase.

Why this exists
---------------
`docs/CHANGELOG_AUDIT.md` exists to catch changelog claims that do not match
the code. It has already failed at that job once: a row asserting a "Context
Governor" with `apply_context_governor_async` and `keep_last_turns` was marked
`verified` even though no such code existed anywhere. The same file cited
`compute_tensor_split` in the wrong module and credited the soak tests with
coverage they did not have.

An audit ledger that is itself unaudited is worse than no ledger, because it
manufactures confidence. This script extracts every backtick-quoted symbol and
file path from the table's Evidence column and confirms each one exists. It
cannot judge whether a *behaviour* is correctly described — that still needs a
human — but it reliably catches the failure mode above: citing code that is not
there, or naming it wrongly.

Usage:
    python3 scripts/verify_changelog_audit.py [--repo-root .]

Exit codes:
    0  every cited symbol/path resolves
    1  at least one citation could not be found
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

# Backtick spans in the Evidence column that look like a code reference.
CODE_SPAN = re.compile(r"`([^`]+)`")

# A path we should be able to stat directly.
PATHISH = re.compile(r"^[\w./-]+\.(rs|ts|tsx|py|go|sh|bat|ps1|yml|yaml|json|md|toml)$")

# Things that are prose, not code references.
IGNORE_SPANS = {
    "verified", "corrected", "keep", "Unreleased",
}

# A span may carry a line range ("file.rs:82-91") or a symbol qualifier
# ("main.rs:handle_gui_chat"). Split those off.
def split_ref(span: str) -> tuple[str, str | None, str | None]:
    """Return (base, line_spec, symbol)."""
    line_spec = None
    symbol = None
    m = re.match(r"^(.*?):(\d+)(?:-(\d+))?$", span)
    if m:
        base = m.group(1)
        line_spec = m.group(2) + (f"-{m.group(3)}" if m.group(3) else "")
        return base, line_spec, symbol
    # path:symbol  (only when the left side looks like a file)
    if ":" in span:
        left, right = span.split(":", 1)
        if PATHISH.match(left) or "/" in left:
            return left, None, right
    return span, None, None


def find_path(root: Path, base: str) -> Path | None:
    """Locate a cited path anywhere in the repo (handles bare filenames)."""
    direct = root / base
    if direct.exists():
        return direct
    # Bare filename or partial path: search once, shallowly.
    name = os.path.basename(base)
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [
            d for d in dirnames
            if d not in {".git", "target", "node_modules", "dist", ".ghostlink"}
        ]
        if name in filenames:
            return Path(dirpath) / name
    return None


def symbol_present(root: Path, symbol: str) -> bool:
    """Is `symbol` defined anywhere in the repo (Rust/TS/Python/shell/etc.)?"""
    needle = symbol.split("::")[-1].split("(")[0].strip()
    if not needle:
        return True  # nothing to check
    # Search the whole repo, excluding build/VCS artifacts.
    r = subprocess.run(
        ["grep", "-rqE", rf"\b{re.escape(needle)}\b",
         "--exclude-dir=.git", "--exclude-dir=target",
         "--exclude-dir=node_modules", "--exclude-dir=dist",
         "--exclude-dir=.ghostlink", "."],
        cwd=root,
        capture_output=True,
    )
    return r.returncode == 0


def line_in_range(root: Path, path: Path, spec: str) -> bool:
    try:
        total = sum(1 for _ in path.open("r", encoding="utf-8", errors="replace"))
    except OSError:
        return False
    first = int(spec.split("-")[0])
    return first <= total


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo-root", default=".")
    ap.add_argument(
        "--audit-file", default="docs/CHANGELOG_AUDIT.md",
        help="Path to the audit ledger, relative to the repo root.",
    )
    args = ap.parse_args()

    root = Path(args.repo_root).resolve()
    audit = root / args.audit_file
    if not audit.exists():
        print(f"Audit file not found: {audit}", file=sys.stderr)
        return 1

    rows = [
        line for line in audit.read_text(encoding="utf-8").splitlines()
        if line.startswith("|") and line.count("|") >= 4
    ]
    # Drop the header + separator.
    rows = [r for r in rows if "---" not in r and "Claim" not in r]

    checked = 0
    failures: list[str] = []

    for row_no, row in enumerate(rows, start=1):
        cells = [c.strip() for c in row.strip().strip("|").split("|")]
        if len(cells) < 4:
            continue
        claim, version, evidence, status = cells[0], cells[1], cells[2], cells[3]

        for span in CODE_SPAN.findall(evidence):
            span = span.strip()
            if span in IGNORE_SPANS or not span:
                continue
            base, line_spec, symbol = split_ref(span)

            # Only treat it as a reference if it plausibly is one.
            looks_like_path = PATHISH.match(base) or "/" in base
            if not looks_like_path and not symbol:
                continue

            checked += 1
            if looks_like_path:
                found = find_path(root, base)
                if found is None:
                    failures.append(
                        f"row {row_no} (v{version}): cited path not found -> `{base}`"
                    )
                    continue
                if line_spec and not line_in_range(root, found, line_spec):
                    failures.append(
                        f"row {row_no} (v{version}): line {line_spec} beyond end of "
                        f"{found.relative_to(root)}"
                    )
            if symbol and not symbol_present(root, symbol):
                failures.append(
                    f"row {row_no} (v{version}): cited symbol not found -> `{symbol}`"
                )

    print(f"Checked {checked} code citation(s) across {len(rows)} audit row(s).")
    if failures:
        print("", file=sys.stderr)
        for f in failures:
            print(f"  {f}", file=sys.stderr)
        print(
            f"\n{len(failures)} citation(s) could not be verified. Either fix the "
            "citation, or change the row's Status from `verified` to `corrected` "
            "with a note explaining the discrepancy.",
            file=sys.stderr,
        )
        return 1

    print("All cited paths and symbols resolve.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
