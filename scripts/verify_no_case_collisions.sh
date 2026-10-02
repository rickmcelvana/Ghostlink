#!/usr/bin/env bash
# Fail if the repo tracks two paths that differ only by letter case.
#
# Why this exists: `.Jules/palette.md` and `.jules/palette.md` were both
# committed for months. On a case-insensitive filesystem (Windows, macOS
# default) those are the *same file*, so a Windows contributor can never
# check out a clean tree and `git status` permanently reports a phantom
# modification they cannot discard. On Linux CI they are two distinct
# files, so nothing caught it. This guard makes the condition fail loudly
# on Linux (where the paths are genuinely distinct) instead of silently
# degrading Windows checkouts.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Native tools (git) on Windows/MSYS do not translate /c/... paths, so pass
# a drive-letter path when the shell's pwd is MSYS-style.
if command -v cygpath >/dev/null 2>&1; then
  ROOT_DIR="$(cygpath -m "$ROOT_DIR")"
fi

dupes="$(git -C "$ROOT_DIR" ls-files \
  | tr '[:upper:]' '[:lower:]' \
  | sort \
  | uniq -d)"

if [[ -n "$dupes" ]]; then
  echo "Found tracked paths that collide when case is ignored:" >&2
  echo "" >&2
  while IFS= read -r lower; do
    [[ -z "$lower" ]] && continue
    echo "  $lower  <- tracked as:" >&2
    git -C "$ROOT_DIR" ls-files | grep -i -x "$lower" | sed 's/^/      /' >&2
  done <<< "$dupes"
  echo "" >&2
  echo "These are the same file on case-insensitive filesystems (Windows," >&2
  echo "macOS), which breaks checkouts there. Keep one casing and delete the" >&2
  echo "other: 'git rm --cached <path>'. If both contain unique content," >&2
  echo "merge it into the canonical path first." >&2
  exit 1
fi

echo "No case-colliding tracked paths found."
