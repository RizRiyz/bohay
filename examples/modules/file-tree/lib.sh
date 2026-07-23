#!/bin/sh
# Shared helpers for the File Tree module. Sourced by render/toggle/open.
#
# State the module keeps between invocations (dock content is not persisted, and
# each click is a fresh subprocess, so anything that must survive lives on disk):
#   $BOHAY_MODULE_STATE_DIR/root       the folder the tree is rooted at
#   $BOHAY_MODULE_STATE_DIR/expanded   newline-separated absolute dir paths
#
# Everything else arrives in the environment, so there is no JSON to parse:
#   BOHAY_BIN_PATH          the running bohay binary (use it, not PATH's `bohay`)
#   BOHAY_WORKSPACE_CWD     the active node's folder
#   BOHAY_MODULE_ROW_VALUE  a clicked row's payload (here: an absolute path)
#   BOHAY_SETTING_*         this module's settings

bohay="${BOHAY_BIN_PATH:-bohay}"
state="${BOHAY_MODULE_STATE_DIR:-/tmp/bohay-file-tree}"
mkdir -p "$state"
root_file="$state/root"
exp_file="$state/expanded"
[ -f "$exp_file" ] || : >"$exp_file"

# The folder the tree is rooted at. `render` (re)writes it from the active node;
# `toggle`/`open` read it so a click acts on the tree the user is looking at,
# not on whatever node happens to be active at click time.
tree_root() {
  if [ -s "$root_file" ]; then cat "$root_file"; else printf '%s' "${BOHAY_WORKSPACE_CWD:-$PWD}"; fi
}

is_expanded() { grep -Fxq "$1" "$exp_file" 2>/dev/null; }

expand()   { is_expanded "$1" || printf '%s\n' "$1" >>"$exp_file"; }
collapse() {
  # Drop the path and anything nested under it, so collapsing a parent also
  # forgets its opened children.
  grep -Fxv "$1" "$exp_file" 2>/dev/null | grep -v "^$1/" >"$exp_file.tmp" 2>/dev/null || :
  mv "$exp_file.tmp" "$exp_file" 2>/dev/null || : >"$exp_file"
}

# Escape the two characters that matter inside a JSON string. Command
# substitution eats a trailing newline, which is exactly what we want here.
json_esc() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
