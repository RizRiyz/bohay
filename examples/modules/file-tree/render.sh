#!/bin/sh
# Walk the (expanded parts of the) tree and push it into the FILES dock.
#
# Only expanded directories are ever descended, so a huge repo costs what you
# open, not the whole thing. Rendering is a flat list: bohay's dock draws rows,
# so depth is indentation baked into each row's text. Folders carry the "toggle"
# action, files the "open" action -- one action per row is all bohay needs.
#
# The walk is an explicit-stack DFS rather than a recursive function: POSIX sh
# has no local variables, so a recursive walk would clobber its own loop state.
set -eu
. "$(dirname "$0")/lib.sh"

# Re-root to the node this was invoked against (startup, a node/tab event, the
# workspace right-click item, or the in-dock refresh row).
printf '%s' "${BOHAY_WORKSPACE_CWD:-$PWD}" >"$root_file"
root=$(tree_root)

show_hidden="${BOHAY_SETTING_SHOW_HIDDEN:-false}"
max_rows="${BOHAY_SETTING_MAX_ROWS:-400}"
TAB=$(printf '\t')
stack="$state/stack.$$"
: >"$stack"

ROWS=""
N=0

add_row() { # text  action  value  [dot]
  esc_t=$(json_esc "$1"); esc_v=$(json_esc "$3")
  if [ -n "${4:-}" ]; then
    ROWS="${ROWS}{\"text\":\"$esc_t\",\"action\":\"$2\",\"value\":\"$esc_v\",\"dot\":\"$4\"},"
  else
    ROWS="${ROWS}{\"text\":\"$esc_t\",\"action\":\"$2\",\"value\":\"$esc_v\"},"
  fi
  N=$((N + 1))
}

indent_for() { i=0; s=""; while [ "$i" -lt "$1" ]; do s="$s  "; i=$((i + 1)); done; printf '%s' "$s"; }

# A directory's immediate children as `depth<TAB>path` lines, dirs before files,
# each group alphabetical (glob expansion is sorted). Dotfiles only when asked;
# `.git` is always hidden.
list_children() { # dir  child-depth
  cd_="$2"
  for e in "$1"/*/ ; do
    [ -d "$e" ] || continue
    q=${e%/}; nm=${q##*/}
    [ "$nm" = ".git" ] && continue
    printf '%s%s%s\n' "$cd_" "$TAB" "$q"
  done
  if [ "$show_hidden" = "true" ]; then
    for e in "$1"/.*/ ; do
      [ -d "$e" ] || continue
      q=${e%/}; nm=${q##*/}
      case "$nm" in .|..) continue ;; esac
      [ "$nm" = ".git" ] && continue
      printf '%s%s%s\n' "$cd_" "$TAB" "$q"
    done
  fi
  for e in "$1"/* ; do
    [ -f "$e" ] || continue
    printf '%s%s%s\n' "$cd_" "$TAB" "$e"
  done
  if [ "$show_hidden" = "true" ]; then
    for e in "$1"/.* ; do
      [ -f "$e" ] || continue
      printf '%s%s%s\n' "$cd_" "$TAB" "$e"
    done
  fi
}

# Push a dir's children onto the LIFO stack in reverse, so they pop in emit order.
push_children() {
  list_children "$1" "$2" | awk '{ a[NR] = $0 } END { for (i = NR; i >= 1; i--) print a[i] }' >>"$stack"
}

# Header row: click to re-root to the active node and repaint.
add_row "⟳  ${root##*/}" refresh "$root" done
push_children "$root" 0

while [ -s "$stack" ] && [ "$N" -lt "$max_rows" ]; do
  line=$(tail -n 1 "$stack")
  sed '$d' "$stack" >"$stack.t" && mv "$stack.t" "$stack"
  depth=${line%%"$TAB"*}
  path=${line#*"$TAB"}
  name=${path##*/}
  ind=$(indent_for "$depth")
  if [ -d "$path" ]; then
    if is_expanded "$path"; then
      add_row "${ind}▾ $name" toggle "$path" working
      push_children "$path" $((depth + 1))
    else
      add_row "${ind}▸ $name" toggle "$path"
    fi
  else
    add_row "${ind}  $name" open "$path"
  fi
done
[ "$N" -lt "$max_rows" ] || add_row "  … (truncated)" refresh "$root"
rm -f "$stack" "$stack.t"

"$bohay" ui dock push --id files --title FILES --rows "[${ROWS%,}]"
