#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md, without its heading — the body
# that becomes that version's GitHub release notes.
#
#   scripts/release-notes.sh 0.9.0
#   scripts/release-notes.sh Unreleased
#
# Exits non-zero when the section is missing or has no entries. Both callers
# rely on that to refuse a release with empty notes: the pre-release-hook in
# release.toml (before the tag exists, where fixing it is cheap) and the
# release workflow (a backstop, after the tag is pushed).
set -euo pipefail

version=${1:?usage: release-notes.sh <version|Unreleased>}
changelog="$(dirname "$0")/../CHANGELOG.md"

# A section ends at the next version heading, or at the link definitions for
# the last one. Leading blank lines are dropped; $(...) drops trailing ones.
notes=$(awk -v heading="## [$version]" '
  $0 == heading || index($0, heading " - ") == 1 { found = 1; next }
  found && (/^## \[/ || /^\[[^]]+\]:[[:space:]]/) { exit }
  found && !started && /^[[:space:]]*$/ { next }
  found { started = 1; print }
' "$changelog")

if [ -z "$notes" ]; then
  echo "error: CHANGELOG.md has no entries under '## [$version]'" >&2
  exit 1
fi

printf '%s\n' "$notes"
