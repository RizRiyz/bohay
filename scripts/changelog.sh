#!/usr/bin/env bash
#
# changelog.sh — categorized Markdown release notes for a tag, built from the
# Conventional-Commit messages since the previous version tag. Used by the release
# workflow; also runnable by hand to preview:
#
#   scripts/changelog.sh v0.1.1          # auto-detects the previous version tag
#   scripts/changelog.sh v0.1.1 v0.1.0   # explicit base
set -euo pipefail

REPO="${GITHUB_REPOSITORY:-RizRiyz/bohay}"
NEW="${1:?usage: changelog.sh <new-tag> [prev-tag]}"

# Previous version tag: newest strict vX.Y.Z that isn't NEW.
PREV="${2:-$(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-version:refname \
             | grep -vxF "$NEW" | head -n1 || true)}"

# Range end: the tag if it exists, else HEAD (so a pre-tag preview works).
END="$NEW"
git rev-parse -q --verify "${NEW}^{commit}" >/dev/null 2>&1 || END="HEAD"
RANGE="${PREV:+$PREV..}$END"
SHA="$(git rev-parse --short "$END")"
BRANCH="${GITHUB_REF_NAME:-$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)}"
case "$BRANCH" in "$NEW" | HEAD | v[0-9]*) BRANCH=main ;; esac # a tag ref → trunk

KNOWN='feat|fix|change|refactor|perf|style|chore|ci|build|docs|test'
NOISE='^(release|homebrew|bump|bum)\b'

# $1 = heading   $2 = ERE of commit types to include
section() {
  local out="" subj hash msg
  while IFS=$'\t' read -r subj hash; do
    [ -n "$subj" ] || continue
    printf '%s' "$subj" | grep -qiE "^($2)(\(.+\))?!?:" || continue
    msg="$(printf '%s' "$subj" | sed -E 's/^[a-zA-Z]+(\(.+\))?!?:[[:space:]]*//')"
    out+="- ${msg} ([\`${hash}\`](https://github.com/${REPO}/commit/${hash}))"$'\n'
  done < <(git log "$RANGE" --no-merges --pretty=tformat:'%s%x09%h')
  [ -n "$out" ] && printf '### %s\n\n%s\n' "$1" "$out"
  return 0
}

# Commits with no recognized prefix (minus release-process noise).
other() {
  local out="" subj hash
  while IFS=$'\t' read -r subj hash; do
    [ -n "$subj" ] || continue
    printf '%s' "$subj" | grep -qiE "^($KNOWN)(\(.+\))?!?:" && continue
    printf '%s' "$subj" | grep -qiE "$NOISE" && continue
    out+="- ${subj} ([\`${hash}\`](https://github.com/${REPO}/commit/${hash}))"$'\n'
  done < <(git log "$RANGE" --no-merges --pretty=tformat:'%s%x09%h')
  [ -n "$out" ] && printf '### 📦 Other\n\n%s\n' "$out"
  return 0
}

# ── header ──
printf '**Built from `%s` on `%s`.**' "$SHA" "$BRANCH"
[ -n "$PREV" ] && printf '  ·  Base stable: `%s`.' "$PREV"
printf '\n\n'

section '✨ Added' 'feat'
section '🔧 Changed' 'change|refactor|perf|style'
section '🐛 Fixed' 'fix'
section '🧹 Maintenance' 'chore|ci|build|docs|test'
other

# ── contributors ──
# %aN (not %an) applies .mailmap, so one person's several git names collapse to one.
authors="$(git log "$RANGE" --no-merges --pretty=tformat:'%aN' | sort -u | sed 's/^/- /')"
[ -n "$authors" ] && printf '### Contributors\n\n%s\n\n' "$authors"

# ── compare footer ──
if [ -n "$PREV" ]; then
  printf '**Full Changelog**: https://github.com/%s/compare/%s...%s\n' "$REPO" "$PREV" "$NEW"
else
  printf '**Full Changelog**: https://github.com/%s/commits/%s\n' "$REPO" "$NEW"
fi
