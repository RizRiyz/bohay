#!/bin/sh
# Fill the BRANCHES dock with the active node's git branches.
#
# Everything this needs arrives in the environment, so there is no JSON parsing:
#   BOHAY_BIN_PATH        the running bohay binary (use it, not `bohay` on PATH)
#   BOHAY_WORKSPACE_CWD   the folder of the node this was invoked against
#   BOHAY_SETTING_LIMIT   the "Branches to show" setting
#   BOHAY_SETTING_SORT    the "Sort by" setting
set -eu

bohay="${BOHAY_BIN_PATH:-bohay}"
repo="${BOHAY_WORKSPACE_CWD:-$PWD}"
limit="${BOHAY_SETTING_LIMIT:-8}"
sort="${BOHAY_SETTING_SORT:-recent}"

# Not a git repo: show one inert row rather than an empty dock.
if ! git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
  "$bohay" ui dock push --id branches --title BRANCHES \
    --rows '[{"text":"not a git repo"}]'
  exit 0
fi

case "$sort" in
  name) order='refname' ;;
  *)    order='-committerdate' ;;
esac

current=$(git -C "$repo" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '')

# Build the rows array. `dot` tints the row's status glyph; `value` is the
# payload handed to the row's action when it is clicked, which is what lets one
# `checkout` action serve every row.
rows=$(git -C "$repo" for-each-ref --format='%(refname:short)' \
         --sort="$order" --count="$limit" refs/heads/ |
  while IFS= read -r branch; do
    [ -n "$branch" ] || continue
    if [ "$branch" = "$current" ]; then dot=working; else dot=idle; fi
    # Escape the two characters that matter inside a JSON string.
    esc=$(printf '%s' "$branch" | sed 's/\\/\\\\/g; s/"/\\"/g')
    printf '{"text":"%s","dot":"%s","action":"checkout","value":"%s"},' \
      "$esc" "$dot" "$esc"
  done)

# Trim the trailing comma and wrap.
"$bohay" ui dock push --id branches --title BRANCHES \
  --rows "[${rows%,}]"
