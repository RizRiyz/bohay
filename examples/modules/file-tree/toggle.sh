#!/bin/sh
# A folder row was clicked: flip its expanded state, then repaint the tree.
set -eu
. "$(dirname "$0")/lib.sh"

path="${BOHAY_MODULE_ROW_VALUE:-}"
[ -n "$path" ] || { "$bohay" ui toast "no folder selected"; exit 1; }

if is_expanded "$path"; then collapse "$path"; else expand "$path"; fi

# Repaint from the stored root, without re-rooting (this click's active node may
# differ from the tree's root; keep the tree the user is looking at). We call
# render's walk by re-running it with the root already pinned, so temporarily
# point BOHAY_WORKSPACE_CWD at the stored root.
BOHAY_WORKSPACE_CWD=$(tree_root) sh "$(dirname "$0")/render.sh"
