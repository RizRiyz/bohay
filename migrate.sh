#!/bin/sh
# Safely migrate the default Bohay 0.10 state into Luvus 0.11.
#
#   curl -fsSL https://luvus.dev/migrate.sh | sh
#
# The migration is copy-only. Existing Bohay state is never moved or deleted,
# and pre-existing Luvus state is preserved as a timestamped backup.
set -eu

err() {
  printf 'error: %s\n' "$1" >&2
  exit 1
}

info() {
  printf '%s\n' "$1"
}

find_binary() {
  name=$1
  shift

  if command -v "$name" >/dev/null 2>&1; then
    command -v "$name"
    return 0
  fi

  for candidate do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  return 1
}

stop_sessions() {
  binary=$1
  state_root=$2

  if [ ! -x "$binary" ]; then
    return 0
  fi

  # Stop named sessions first so stopping the default session cannot strand one.
  for session_dir in "$state_root"/sessions/*; do
    [ -d "$session_dir" ] || continue
    session_name=${session_dir##*/}
    "$binary" --session "$session_name" server stop >/dev/null 2>&1 || :
  done
  "$binary" server stop >/dev/null 2>&1 || :
}

restore_previous_state() {
  reason=$1
  trap - HUP INT TERM

  stop_sessions "$luvus_bin" "$current"

  if [ -e "$current" ]; then
    failed="${current}.failed-migration-${stamp}-$$"
    if mv "$current" "$failed"; then
      printf 'Incomplete Luvus state preserved at %s\n' "$failed" >&2
    else
      printf 'warning: could not preserve incomplete Luvus state at %s\n' "$current" >&2
    fi
  fi

  if [ -n "$backup" ] && [ -e "$backup" ]; then
    if mv "$backup" "$current"; then
      printf 'Previous Luvus state restored to %s\n' "$current" >&2
    else
      printf 'warning: restore the previous Luvus state manually from %s\n' "$backup" >&2
    fi
  fi

  err "$reason"
}

[ -n "${HOME:-}" ] || err 'HOME is not set'
[ "$HOME" != "/" ] || err 'refusing to use / as HOME'

if [ "${BOHAY_ENV:-}" = "1" ] || [ "${LUVUS_ENV:-}" = "1" ]; then
  err 'run this command from a normal terminal outside Bohay or Luvus; migration stops the old server and its panes'
fi

if [ -n "${BOHAY_HOME:-}" ] || [ -n "${LUVUS_HOME:-}" ]; then
  err 'custom BOHAY_HOME/LUVUS_HOME values are not migrated automatically; unset them and rerun for the default homes'
fi

legacy="$HOME/.bohay"
current="$HOME/.luvus"
marker="$current/.migrated-from-bohay-0.10"

[ ! -L "$legacy" ] || err "$legacy is a symbolic link; migrate it manually"
[ ! -L "$current" ] || err "$current is a symbolic link; migrate it manually"

if [ -f "$marker" ]; then
  info "Bohay state was already migrated to $current"
  exit 0
fi

recognized=false
for entry in config.json session.json sessions orch.json modules.json modules manifests worktrees; do
  if [ -e "$legacy/$entry" ]; then
    recognized=true
    break
  fi
done
[ "$recognized" = true ] || err "no Bohay state found at $legacy"

if [ -n "${LUVUS_MIGRATE_LUVUS_BIN:-}" ]; then
  luvus_bin=$LUVUS_MIGRATE_LUVUS_BIN
else
  luvus_bin=$(find_binary luvus \
    "$HOME/.local/bin/luvus" \
    "$HOME/.cargo/bin/luvus" \
    /opt/homebrew/bin/luvus \
    /usr/local/bin/luvus) || err 'Luvus is not installed; run curl -fsSL https://luvus.dev/install.sh | sh first'
fi
[ -x "$luvus_bin" ] || err "Luvus binary is not executable: $luvus_bin"

if [ -n "${LUVUS_MIGRATE_BOHAY_BIN:-}" ]; then
  bohay_bin=$LUVUS_MIGRATE_BOHAY_BIN
else
  bohay_bin=$(find_binary bohay \
    "$HOME/.local/bin/bohay" \
    "$HOME/.cargo/bin/bohay" \
    /opt/homebrew/bin/bohay \
    /usr/local/bin/bohay 2>/dev/null || true)
fi

info 'Stopping Luvus and Bohay servers so their final snapshots are complete…'
if [ -e "$current" ]; then
  stop_sessions "$luvus_bin" "$current"
fi
if [ -n "$bohay_bin" ]; then
  stop_sessions "$bohay_bin" "$legacy"
fi

# Both server-stop paths acknowledge after processing the request. Give detached
# processes a brief moment to close their sockets before Luvus checks them.
sleep 1

stamp=$(date '+%Y%m%d-%H%M%S')
backup=''
if [ -e "$current" ]; then
  backup="${current}.before-bohay-migration-${stamp}-$$"
  mv "$current" "$backup" || err "could not preserve existing Luvus state at $backup"
  info "Existing Luvus state preserved at $backup"
fi

trap 'restore_previous_state "migration interrupted; no previous Luvus state was discarded"' HUP INT TERM

# The Luvus binary owns the actual copy policy and path rewriting. Keeping that
# logic in one place prevents this downloaded script from copying sockets,
# locks, symlinks, skill caches, or stale Bohay-managed module paths.
unset BOHAY_ENV BOHAY_HOME BOHAY_SOCKET_PATH BOHAY_PANE_ID BOHAY_SESSION
unset LUVUS_ENV LUVUS_HOME LUVUS_SOCKET_PATH LUVUS_PANE_ID LUVUS_SESSION

if ! "$luvus_bin" server start; then
  restore_previous_state 'Luvus could not start the migration'
fi

if [ ! -f "$marker" ]; then
  restore_previous_state 'migration did not complete; a Bohay server may still be running'
fi

trap - HUP INT TERM

printf '\nMigrated Bohay state to %s\n' "$current"
printf 'Original Bohay state remains at %s\n' "$legacy"
if [ -n "$backup" ]; then
  printf 'Previous Luvus state remains backed up at %s\n' "$backup"
fi
printf 'Run: luvus\n'
