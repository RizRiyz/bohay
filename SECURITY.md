# Security Policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Instead, use
GitHub's private reporting: **[Report a vulnerability](https://github.com/RizRiyz/bohay/security/advisories/new)**
(Security tab → *Report a vulnerability*).

You can expect an acknowledgement within a few days. Please include a
reproduction and your assessment of impact if you can.

## Scope & trust model

bohay is a **single-user, local-trust** tool:

- Its control sockets live in `~/.bohay/` (created `0700`, sockets `0600`) and
  grant full control of your panes — protect that directory like your shell.
- Agents and modules you run inside panes execute with your privileges, like
  any terminal. Module *installs* are previewed and confirmed before anything
  runs; module builds get a scrubbed environment.
- Remote attach rides plain `ssh` — bohay adds no network listener of its own.

Reports that cross these boundaries (e.g. another local user reaching the
socket, a repo's data executing code without confirmation, escaping the
module sandbox paths) are exactly what we want to hear about.

## Supported versions

Only the latest release receives security fixes.
