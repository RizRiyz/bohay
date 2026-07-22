#!/usr/bin/env bash
#
# changelog.sh — release notes for a tag.
#
# `changelog/<tag>.md` in the repo is the **source of truth**. It is what the
# GitHub Release publishes and what bohay.dev/changelog renders, so the notes are
# version-controlled, reviewable in a PR, and identical everywhere.
#
#   scripts/changelog.sh v0.8.1            # print the notes (curated file if it
#                                          # exists, else generated from commits)
#   scripts/changelog.sh v0.8.1 --write    # create changelog/v0.8.1.md if absent
#   scripts/changelog.sh v0.8.1 --write --force   # regenerate, discarding edits
#   scripts/changelog.sh v0.8.1 v0.8.0     # explicit base tag
#
# Printing strips the YAML front matter, since a GitHub Release body has no use
# for it — the website reads it for the version/date.
set -euo pipefail

REPO="${GITHUB_REPOSITORY:-RizRiyz/bohay}"
NEW=""
PREV=""
WRITE=0
FORCE=0
for arg in "$@"; do
  case "$arg" in
    --write) WRITE=1 ;;
    --force) FORCE=1 ;;
    -*) printf 'unknown flag: %s\n' "$arg" >&2; exit 2 ;;
    *) if [ -z "$NEW" ]; then NEW="$arg"; else PREV="$arg"; fi ;;
  esac
done
[ -n "$NEW" ] || { echo "usage: changelog.sh <new-tag> [prev-tag] [--write] [--force]" >&2; exit 2; }

ROOT="$(git rev-parse --show-toplevel)"
FILE="$ROOT/changelog/$NEW.md"

# Strip YAML front matter, then any leading blank lines.
strip_front_matter() {
  awk 'NR==1 && $0=="---" {fm=1; next} fm && $0=="---" {fm=0; next} !fm' \
    | awk 'NF {p=1} p'
}

# Print an existing curated file verbatim — never regenerate over hand-written notes.
if [ "$WRITE" = 0 ] && [ -f "$FILE" ]; then
  strip_front_matter < "$FILE"
  exit 0
fi
if [ "$WRITE" = 1 ] && [ -f "$FILE" ] && [ "$FORCE" = 0 ]; then
  printf 'changelog/%s.md already exists — edit it, or pass --force to regenerate.\n' "$NEW" >&2
  exit 0
fi

# Previous version tag: newest strict vX.Y.Z that isn't NEW.
[ -n "$PREV" ] || PREV="$(git tag --list 'v[0-9]*.[0-9]*.[0-9]*' --sort=-version:refname \
                          | grep -vxF "$NEW" | head -n1 || true)"

# Range end: the tag if it exists, else HEAD (so a pre-tag preview works).
END="$NEW"
git rev-parse -q --verify "${NEW}^{commit}" >/dev/null 2>&1 || END="HEAD"
RANGE="${PREV:+$PREV..}$END"

KNOWN='feat|fix|change|refactor|perf|style|chore|ci|build|docs|test'
# Release plumbing and dependency churn are not user-facing news.
NOISE='^(release|homebrew|bump|bum|merge)\b|^chore\(deps'

# Turn a commit subject into a readable bullet: drop the Conventional-Commit
# type, keep any scope as a lead-in, and capitalize the first letter.
polish() {
  local subj="$1" scope body
  scope="$(printf '%s' "$subj" | sed -nE 's/^[a-zA-Z]+\(([^)]+)\)!?:.*/\1/p')"
  body="$(printf '%s' "$subj" | sed -E 's/^[a-zA-Z]+(\([^)]*\))?!?:[[:space:]]*//')"
  # Uppercase the first letter portably: sed's \U is GNU-only (macOS ships BSD
  # sed) and ${var^} needs bash 4 (macOS ships 3.2).
  body="$(printf '%s' "${body%"${body#?}"}" | tr '[:lower:]' '[:upper:]')${body#?}"
  if [ -n "$scope" ]; then printf '**%s:** %s' "$scope" "$body"; else printf '%s' "$body"; fi
}

# $1 = heading   $2 = ERE of commit types to include
section() {
  local out="" subj hash
  while IFS=$'\t' read -r subj hash; do
    [ -n "$subj" ] || continue
    printf '%s' "$subj" | grep -qiE "^($2)(\(.+\))?!?:" || continue
    printf '%s' "$subj" | grep -qiE "$NOISE" && continue
    out+="- $(polish "$subj") ([\`${hash}\`](https://github.com/${REPO}/commit/${hash}))"$'\n'
  done < <(git log "$RANGE" --no-merges --pretty=tformat:'%s%x09%h')
  [ -n "$out" ] && printf '### %s\n\n%s\n' "$1" "$out"
  return 0
}

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

notes() {
  # Front matter — the website reads version + date from here; printing strips it.
  printf -- '---\nversion: %s\ndate: %s\n---\n\n' "$NEW" "$(date -u +%Y-%m-%d)"

  # A prose lead. Generated notes can only ever restate commit subjects, so leave
  # an explicit prompt: these notes exist to *explain the work*, not to list it.
  # Delete the blockquote once written.
  printf '> _Write 2-4 sentences: what this release is about and who should care._\n'
  printf '>\n'
  printf '> _Then expand each bullet below into what it actually does for the reader —\n'
  printf '> the behaviour, why it changed, and anything to watch out for. Drop bullets\n'
  printf '> that are not user-facing. Delete this note when done._\n\n'

  section '✨ Added' 'feat'
  section '🔧 Changed' 'change|refactor|perf|style'
  section '🐛 Fixed' 'fix'
  section '🧹 Maintenance' 'chore|ci|build|docs|test'
  other

  # %aN (not %an) applies .mailmap, so one person's several git names collapse to one.
  local authors
  authors="$(git log "$RANGE" --no-merges --pretty=tformat:'%aN' | sort -u | sed 's/^/- /')"
  [ -n "$authors" ] && printf '### Contributors\n\n%s\n\n' "$authors"

  if [ -n "$PREV" ]; then
    printf '**Full Changelog**: https://github.com/%s/compare/%s...%s\n' "$REPO" "$PREV" "$NEW"
  else
    printf '**Full Changelog**: https://github.com/%s/commits/%s\n' "$REPO" "$NEW"
  fi
}

if [ "$WRITE" = 1 ]; then
  mkdir -p "$ROOT/changelog"
  notes > "$FILE"
  printf 'wrote changelog/%s.md — edit it before releasing.\n' "$NEW" >&2
else
  # Same strip as the curated path, so both produce an identical release body.
  notes | strip_front_matter
fi
