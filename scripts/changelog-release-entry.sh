#!/usr/bin/env bash
# Insert a release entry into CHANGELOG.md listing the dependencies Dependabot
# updated in the merge being released. Called by auto-release.yml.
#
# Required env:
#   NEXT  new version, e.g. 0.2.1
#   PREV  previous version, e.g. 0.2.0
#   REPO  owner/name, e.g. Martin8412/natpmp-rs
# Optional env:
#   NEW_REF    the merge commit to read the dependency list from (default HEAD)
#   CHANGELOG  path to the changelog                              (default CHANGELOG.md)
set -euo pipefail

: "${NEXT:?}" "${PREV:?}" "${REPO:?}"
NEW_REF="${NEW_REF:-HEAD}"
CHANGELOG="${CHANGELOG:-CHANGELOG.md}"

if [ ! -f "$CHANGELOG" ] || ! grep -q '^## \[Unreleased\]$' "$CHANGELOG"; then
  echo "No $CHANGELOG [Unreleased] section; skipping changelog update." >&2
  exit 0
fi

# Dependabot records each update in the commit body as "Bumps `name` from X to
# Y" (single update) or "Updates `name` from X to Y" (one line per dependency in
# a grouped update). Turn those into changelog bullets.
notes="$(git show -s --format=%b "$NEW_REF" \
  | sed -nE 's/^(Bumps|Updates) `([^`]+)` from ([^ ]+) to ([^ ]+).*/- Bump `\2` from \3 to \4/p' \
  | sort -u)"

# Fall back to a generic line if the body didn't parse as expected.
[ -n "$notes" ] || notes="- Update Rust dependencies."

date="$(date -u +%Y-%m-%d)"
url="https://github.com/${REPO}"

section="$(mktemp)"
printf '\n## [%s] - %s\n\n### Changed\n\n%s\n' "$NEXT" "$date" "$notes" > "$section"

# Splice the new section in right after the [Unreleased] heading.
awk -v f="$section" '
  { print }
  /^## \[Unreleased\]$/ && !done {
    while ((getline line < f) > 0) print line
    close(f); done = 1
  }
' "$CHANGELOG" > "${CHANGELOG}.tmp" && mv "${CHANGELOG}.tmp" "$CHANGELOG"
rm -f "$section"

# Rewrite the [Unreleased] compare link and add the new version's link below it.
awk -v url="$url" -v nv="$NEXT" -v pv="$PREV" '
  /^\[Unreleased\]:/ {
    print "[Unreleased]: " url "/compare/v" nv "...HEAD"
    print "[" nv "]: " url "/compare/v" pv "...v" nv
    next
  }
  { print }
' "$CHANGELOG" > "${CHANGELOG}.tmp" && mv "${CHANGELOG}.tmp" "$CHANGELOG"
