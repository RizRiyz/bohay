#!/usr/bin/env python3
"""Post a webhook when an agent goes blocked or finishes.

bohay runs this as a plain subprocess, so there is no SDK to import. Two ways in:

  * the flat vars, easiest from any language:
      BOHAY_PANE_ID / BOHAY_PANE_AGENT / BOHAY_PANE_STATUS
      BOHAY_WORKSPACE_CWD, BOHAY_SETTING_WEBHOOK, BOHAY_SETTING_NOTIFY_ON, ...
  * the full snapshot, when you want more:
      BOHAY_MODULE_CONTEXT_JSON, and for an event hook BOHAY_MODULE_EVENT_JSON

Talk back to bohay through BOHAY_BIN_PATH, never a bare `bohay` on PATH -- that
keeps the module working on Windows named pipes as well as Unix sockets.
"""

import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

BOHAY = os.environ.get("BOHAY_BIN_PATH", "bohay")


def bohay(*args: str) -> None:
    """Call back into bohay, ignoring failures (a module must never wedge the UI)."""
    try:
        subprocess.run([BOHAY, *args], check=False, capture_output=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        pass


def main() -> int:
    forced = "--force" in sys.argv

    # Settings arrive pre-resolved: manifest defaults with the user's choices on
    # top, already type-checked and clamped by bohay.
    webhook = os.environ.get("BOHAY_SETTING_WEBHOOK", "").strip()
    notify_on = os.environ.get("BOHAY_SETTING_NOTIFY_ON", "blocked")
    want_toast = os.environ.get("BOHAY_SETTING_TOAST", "true") == "true"

    agent = os.environ.get("BOHAY_PANE_AGENT") or "agent"
    status = os.environ.get("BOHAY_PANE_STATUS") or "unknown"
    pane = os.environ.get("BOHAY_PANE_ID") or "?"

    # An event hook prefers the event payload, which describes the pane that
    # actually changed rather than the one in focus.
    raw = os.environ.get("BOHAY_MODULE_EVENT_JSON")
    if raw:
        try:
            event = json.loads(raw)
            agent = event.get("agent") or agent
            status = event.get("status") or status
            pane = event.get("pane") or pane
        except json.JSONDecodeError:
            pass

    # Right-click always pings; an event only pings for the states asked for.
    if not forced:
        wanted = {"both": {"blocked", "done"}}.get(notify_on, {notify_on})
        if status not in wanted:
            return 0

    where = os.path.basename(os.environ.get("BOHAY_WORKSPACE_CWD", "") or "") or "bohay"
    message = f"{agent} is {status} in {where} (pane {pane})"

    if want_toast:
        bohay("ui", "toast", message)

    if not webhook:
        # Nothing configured yet: point the user at where to set it, once.
        if forced:
            bohay("ui", "toast", "set a Webhook URL in Settings > Modules")
        return 0

    body = json.dumps({"text": message}).encode()
    request = urllib.request.Request(
        webhook, data=body, headers={"Content-Type": "application/json"}
    )
    try:
        urllib.request.urlopen(request, timeout=10).close()
    except (urllib.error.URLError, OSError) as err:
        # stderr lands in `bohay module log`, which is where you debug a module.
        print(f"webhook failed: {err}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
