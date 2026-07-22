// Append the pane's selected text to the notes file.
//
// Selecting text in a pane and right-clicking puts that selection in the
// invocation context, so a module can act on what the user highlighted.
const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const bohay = process.env.BOHAY_BIN_PATH ?? "bohay";
const toast = (text) => spawnSync(bohay, ["ui", "toast", text], { encoding: "utf8" });

const context = JSON.parse(process.env.BOHAY_MODULE_CONTEXT_JSON ?? "{}");
const selection = (context.selection ?? "").trim();

if (!selection) {
  toast("select some text first");
  process.exit(0);
}

// Durable, user-visible state belongs in the state dir. Never write it into
// BOHAY_MODULE_ROOT: a git-installed module's root is a managed checkout that
// a reinstall replaces.
const stateDir = process.env.BOHAY_MODULE_STATE_DIR ?? process.cwd();
fs.mkdirSync(stateDir, { recursive: true });

const file = path.join(stateDir, "notes.md");
const where = context.pane?.cwd || context.workspace?.cwd || "";
fs.appendFileSync(file, `\n## from ${where}\n\n${selection}\n`);

toast(`saved ${selection.split("\n").length} line(s) to notes`);
