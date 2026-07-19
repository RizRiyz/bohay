# Contributing to bohay

Thanks for helping make bohay better! Issues and PRs are welcome.

## Getting started

```bash
git clone https://github.com/RizRiyz/bohay
cd bohay
cargo build            # pure Rust, no C toolchain needed (Rust ≥ 1.82)
cargo run -- --local   # run client + server in one process (dev escape hatch)
```

Debug builds keep their state in `~/.bohay-dev/`, so hacking on bohay never
touches your real session in `~/.bohay/`.

## Before you open a PR

CI runs these on every PR — save a round-trip and run them locally:

```bash
cargo test                                  # unit + off-screen render tests (no tty needed)
cargo clippy --all-targets -- -D warnings   # lints (warnings are errors in CI)
cargo fmt --all --check                     # formatting
```

Tests render the full UI into an off-screen buffer and spawn real PTYs, so
layout, VT, and draw paths are exercised without a terminal. Please add a test
with any behavior change — `cargo test <substring>` runs a single test.

## Guidelines

- **Performance first.** bohay's promise is a fast, smooth terminal. Anything
  on the render or event hot path gets scrutiny; avoid per-frame allocations,
  unbounded scans, or blocking I/O on the event loop (shell-outs and filesystem
  scans belong on worker threads).
- **Keep state pure.** App state is separate from runtime/IO (one event loop,
  one timer). Match the existing module layout and code style around you.
- **Commit messages** follow Conventional Commits (`feat:`, `fix:`, `perf:`,
  `docs:`, `test:`, `chore:` …) — the release changelog is generated from them.
- **Windows counts.** `cargo check --target x86_64-pc-windows-gnu` should stay
  green; OS-specific code lives in `platform.rs` behind `cfg` gates.

## Extending bohay instead

If you want to add functionality *around* bohay (panels, integrations,
automations), a **module** may be a better fit than a core PR — see
[MODULE-GUIDE.md](MODULE-GUIDE.md). Modules are separate repos, any language,
no SDK.

## Reporting bugs

Please include your OS/terminal, `bohay server status` + `bohay doctor` output,
and steps to reproduce. For security issues, see [SECURITY.md](SECURITY.md) -
please don't open a public issue.
