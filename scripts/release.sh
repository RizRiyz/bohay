#!/usr/bin/env bash
#
# release.sh — cut a new bohay version to crates.io, GitHub (binaries), and Homebrew.
#
#   scripts/release.sh 0.1.1             # full release (prompts before publishing)
#   scripts/release.sh 0.1.1 --dry-run   # bump + verify only, then revert — no release
#   scripts/release.sh 0.1.1 --yes       # skip the confirmation prompt
#
# Prereqs:  `cargo login` done · `gh auth login` · push access to the repo.
# Tap:      the Homebrew formula in ./homebrew-bohay (or $BOHAY_TAP_DIR) — the real
#           `brew install RizRiyz/bohay/bohay` source — is bumped & pushed too.
set -euo pipefail

REPO="RizRiyz/bohay"

die()  { printf '\033[31merror:\033[0m %s\n' "$1" >&2; exit 1; }
step() { printf '\n\033[36m▸ %s\033[0m\n' "$1"; }
sha256() { if command -v shasum >/dev/null; then shasum -a 256; else sha256sum; fi | cut -d' ' -f1; }
# Rewrite the formula in place for $TAG. The formula ships **prebuilt binaries**
# (one url+sha256 per platform) plus a source fallback for Intel macs, so this
# has to bump the version, every url, and every checksum — each from that
# platform's published `.sha256` asset. $SHA (the source tarball's checksum) is
# set before calling.
#
# Ordering matters: the binary assets only exist once the release workflow has
# built them, so `wait_for_assets` runs first.
FORMULA_TARGETS="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl"

# Block until every prebuilt asset for $TAG is published (the workflow builds
# them after the tag push). Gives up after ~10 minutes rather than hanging.
wait_for_assets() {
  local waited=0
  while [ "$waited" -lt 600 ]; do
    local missing=0
    for t in $FORMULA_TARGETS; do
      gh release view "$TAG" --repo "$REPO" --json assets \
        --jq ".assets[].name" 2>/dev/null | grep -qx "bohay-$TAG-$t.sha256" || missing=1
    done
    [ "$missing" = 0 ] && return 0
    printf '  waiting for release binaries… (%ss)\r' "$waited"
    sleep 15
    waited=$((waited + 15))
  done
  die "release assets for $TAG never appeared — bump the tap by hand once the workflow finishes"
}

# The published checksum for one target.
asset_sha() {
  gh release download "$TAG" --repo "$REPO" --pattern "bohay-$TAG-$1.sha256" -O - 2>/dev/null \
    | awk '{print $1}'
}

bump_formula() {
  local f="$1" t sha
  # version + the Intel-mac source fallback
  perl -0pi -e "s/^  version \"[0-9.]+\"/  version \"$VERSION\"/m" "$f"
  perl -0pi -e "s{archive/refs/tags/v[0-9.]+\.tar\.gz}{archive/refs/tags/$TAG.tar.gz}g" "$f"
  # Each prebuilt: rewrite its url to $TAG, then the sha256 on the line after it.
  for t in $FORMULA_TARGETS; do
    sha="$(asset_sha "$t")"
    [ -n "$sha" ] || die "no published checksum for $t — cannot bump the formula"
    perl -0pi -e "s{releases/download/v[0-9.]+/bohay-v[0-9.]+-$t\.tar\.gz}{releases/download/$TAG/bohay-$TAG-$t.tar.gz}g" "$f"
    perl -0pi -e "s{(bohay-$TAG-$t\.tar\.gz\"\n\s*sha256 \")[0-9a-f]{64}}{\${1}$sha}s" "$f"
  done
  # The source fallback's checksum is the last one still on the old value.
  perl -0pi -e "s{(archive/refs/tags/$TAG\.tar\.gz\"\n\s*sha256 \")[0-9a-f]{64}}{\${1}$SHA}s" "$f"
  # Nothing may still point at an older tag.
  ! grep -qE "v[0-9]+\.[0-9]+\.[0-9]+" "$f" || grep -qE "$TAG" "$f" \
    || die "formula still references an old tag after the bump"
}

VERSION="${1:-}"
MODE="${2:-}"
[ -n "$VERSION" ] || die "usage: scripts/release.sh X.Y.Z [--dry-run|--yes]"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be semver X.Y.Z (got '$VERSION')"
TAG="v$VERSION"
cd "$(git rev-parse --show-toplevel)"

# Self-heal: if we bail out (failed check, abort, dry-run) before the release is
# committed, undo the version bump so the tree is never left half-updated.
committed=0
trap '[ "$committed" = 1 ] || git checkout -- Cargo.toml Cargo.lock 2>/dev/null || true' EXIT

step "Preconditions"
[ "$(git branch --show-current)" = "main" ] || die "not on main"
[ -z "$(git status --porcelain)" ] || die "working tree is dirty — commit or stash first"
git fetch --tags --quiet
git rev-parse "$TAG" >/dev/null 2>&1 && die "$TAG already exists"
CURRENT=$(grep -m1 '^version = ' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
echo "  $CURRENT  →  $VERSION"
# `changelog/<tag>.md` is the single source the GitHub Release *and* bohay.dev
# both render, so it has to exist and be committed before we tag. Generate the
# skeleton and stop, rather than shipping a release with auto-listed commits.
CHANGELOG="changelog/$TAG.md"
if [ ! -f "$CHANGELOG" ]; then
  bash scripts/changelog.sh "$TAG" --write
  die "wrote $CHANGELOG — edit it, commit it, then re-run this script"
fi
if grep -q 'Then delete this note' "$CHANGELOG"; then
  die "$CHANGELOG still has the placeholder note — write the summary, commit, then re-run"
fi
echo "  notes: $CHANGELOG"
# The Homebrew tap (its own git repo): the in-repo clone by default.
TAP="${BOHAY_TAP_DIR:-homebrew-bohay}"
if [ -f "$TAP/Formula/bohay.rb" ]; then
  [ -z "$(git -C "$TAP" status --porcelain)" ] || die "tap '$TAP' has uncommitted changes"
  echo "  tap: $TAP  (will bump + push)"
else
  echo "  tap: none at '$TAP' — Homebrew step will print manual instructions"
fi

step "Bump Cargo.toml + Cargo.lock"
# Only the [package] version is at the start of a line; deps use `name = "..."`.
perl -0pi -e "s/^version = \"[0-9]+\.[0-9]+\.[0-9]+\"/version = \"$VERSION\"/m" Cargo.toml
cargo check --quiet                       # syncs Cargo.lock's bohay version
grep -q "^version = \"$VERSION\"" Cargo.toml || die "Cargo.toml bump failed"

step "Verify (fmt · clippy · test · publish dry-run)"
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
# --allow-dirty: the version bump isn't committed yet at this point. This is only
# a build/package check; the REAL `cargo publish` below runs after the commit on a
# clean tree, so the published artifact still matches a committed state.
cargo publish --dry-run --allow-dirty

step "Release notes preview (what the workflow will publish on the GitHub Release)"
bash scripts/changelog.sh "$TAG"

if [ "$MODE" = "--dry-run" ]; then
  step "Dry run OK — everything passed. Re-run without --dry-run to release."
  exit 0 # the trap reverts the bump
fi

if [ "$MODE" != "--yes" ]; then
  printf "\nRelease \033[1m%s\033[0m to crates.io + GitHub + Homebrew. Continue? [y/N] " "$TAG"
  read -r ans
  [ "$ans" = "y" ] || [ "$ans" = "Y" ] || die "aborted" # the trap reverts the bump
fi

step "Commit + tag"
git add Cargo.toml Cargo.lock
git commit -m "release: $TAG"
committed=1 # past here the bump is committed — the trap must not revert it
git tag -a "$TAG" -m "$TAG"

step "Push (triggers the release workflow → binaries)"
git push origin main
git push origin "$TAG"

step "Publish to crates.io"
cargo publish

step "Homebrew formula (source tarball is ready the instant the tag is pushed)"
TARBALL="https://github.com/$REPO/archive/refs/tags/$TAG.tar.gz"
SHA=$(curl -fsSL --retry 5 --retry-delay 2 "$TARBALL" | sha256)
[ -n "$SHA" ] || die "could not fetch + hash $TARBALL"
echo "  sha256: $SHA"

# The tap (its own repo) is the single source of truth — `brew install` pulls it.

if [ -f "$TAP/Formula/bohay.rb" ]; then
  step "Update tap ($TAP)"
  wait_for_assets
  bump_formula "$TAP/Formula/bohay.rb"
  git -C "$TAP" add Formula/bohay.rb
  git -C "$TAP" commit -m "bohay $TAG"
  # The notch workflow pushes its cask to this same repo during the release, so
  # land on top of whatever it did instead of being rejected.
  git -C "$TAP" pull --rebase --quiet
  git -C "$TAP" push
  echo "  ✓ tap pushed — brew install $REPO/bohay now serves $TAG"
else
  step "Tap '$TAP' not found — finish Homebrew by hand:"
  echo "    git clone git@github.com:${REPO%%/*}/homebrew-bohay.git"
  echo "    # in it: set url → .../$TAG.tar.gz and sha256 → $SHA, then commit & push"
fi

step "Done — $TAG released 🎉"
echo "  cargo:    cargo install bohay"
echo "  binaries: https://github.com/$REPO/releases/tag/$TAG  (workflow building now)"
echo "  brew:     brew install $REPO/bohay"
