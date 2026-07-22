// The `notes` pane entrypoint: a tiny scratch pad running in a real bohay pane.
//
// A pane command owns a terminal. Read stdin, write stdout, and exit when you
// are done -- bohay closes the pane when the process exits.
const fs = require("node:fs");
const path = require("node:path");
const readline = require("node:readline");

const stateDir = process.env.BOHAY_MODULE_STATE_DIR ?? process.cwd();
fs.mkdirSync(stateDir, { recursive: true });
const file = path.join(stateDir, "notes.md");

function show() {
  const body = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
  console.clear();
  console.log("notes  ·  type a line to append  ·  :r reload  ·  :q quit");
  console.log("─".repeat(56));
  console.log(body.trim() || "(empty)");
  console.log("─".repeat(56));
}

show();
const rl = readline.createInterface({ input: process.stdin, output: process.stdout, prompt: "> " });
rl.prompt();

rl.on("line", (line) => {
  const text = line.trim();
  if (text === ":q") return rl.close();
  if (text === ":r") {
    show();
  } else if (text) {
    fs.appendFileSync(file, `${text}\n`);
    show();
  }
  rl.prompt();
});

rl.on("close", () => process.exit(0));
