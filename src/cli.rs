//! CLI client (M4): `bohay pane …` / `bohay ping` / `bohay events` connect to
//! the session socket, send one JSON request, and print the reply. See docs/08.

use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// Returns true if `argv[1]` is a CLI noun we handle (so `main` should not
/// launch the TUI).
pub fn is_cli(args: &[String]) -> bool {
    matches!(
        args.get(1).map(String::as_str),
        Some(
            "ping"
                | "pane"
                | "node"
                | "workspace"
                | "tab"
                | "agent"
                | "ui"
                | "events"
                | "module"
                | "git"
                | "worktree"
                | "task"
                | "lease"
                | "wait"
                | "help"
                | "doctor"
        )
    )
}

const USAGE: &str = "\
bohay — terminal workspace manager for AI coding agents

usage: bohay <command> [args]

  (no args)            launch / attach the TUI
  help                 show this help
  doctor               check optional external tools (git, gh, …)
  ping                 check the server

workspaces:
  workspace list             list workspaces
  workspace new              create a workspace in the current directory
  workspace open <path>      open <path> as a workspace (or focus it if already open)
  workspace focus <i>        focus workspace i (0-based)
  workspace close [<i>]      close a workspace (default: active)

tabs:
  tab list                   list tabs in the current workspace
  tab new                    new tab
  tab focus <n>              focus tab n (1-based)
  tab close [<n>]            close a tab (default: active)

panes / agents:
  pane list                  list panes in the current tab
  pane split [<id>] [--down] split a pane (default: side by side)
  pane focus <id>            focus a pane (jumps to its workspace/tab)
  pane run [<id>] <cmd...>   run a command in a pane
  pane send [<id>] <text>    send raw text to a pane
  pane read [<id>]           print a pane's recent output
  pane status [<id>]         print a pane's agent status (any workspace)
  pane close [<id>]          close a pane
  agent list                 list every agent across all workspaces/tabs
  agent sessions             list resumable sessions found on disk
  agent resume <id>          reopen a resumable session into a pane
  wait output <id> --match <text> [--timeout <s>]    block until output appears
  wait agent-status <id> --status done|blocked|working|idle [--timeout <s>]
  attach <id>                open the TUI into a single fullscreen pane

appearance:
  ui sidebar [--side left|right] --width <n>     set a sidebar's width (columns)
  ui sidebar [--side left|right] --hide|--show   toggle a sidebar
  ui dock list               list docks and which side each is on
  ui dock move --id <id> --side left|right       place a dock on a side
  ui dock push --id <id> [--title <t>] [--side left|right] [--rows <json>]
                             feed a module's sidebar dock its rows (JSON array,
                             or piped on stdin) — see docs/29 + the website

modules (extensions):
  module search [<query>]    find modules published to the `bohay-module` GitHub topic
  module list                list installed modules
  module info <id>           show a module's actions / panes / events / source
  module link <path>         register a local module dir (--disabled to skip enabling)
  module install <owner>/<repo>[/sub] [--ref REF] [--yes]   install from GitHub
  module unlink <id>         remove a module from the registry
  module uninstall <id>      unlink + delete a git-installed module's checkout
  module enable <id> | disable <id>
  module actions             list every action across modules
  module run <id> <action>   invoke a module action (captures + logs output)
  module pane open <id> <entrypoint> [--placement split|overlay|tab]
  module pane focus <pane> | close <pane>
  module log [<id>]          tail module command logs (--limit N)
  module config-dir <id>     print/create a module's config dir

git:
  git status                 branch, ahead/behind, working tree of the current workspace
  git branches               local branches with tracking
  git log [--limit N]        recent commits
  git open [<workspace>]     open the git tab for a workspace

worktrees:
  worktree list              list the current repo's worktrees
  worktree create <branch>   create a worktree + workspace for <branch>
  worktree open <path>       open an existing worktree as a workspace
  worktree remove <path>     remove a worktree (its branch is kept)

orchestration (multiple agents, one project — docs/22):
  task add \"<title>\" [--paths <glob>...] [--dep <id>...] [--gate <cmd>]
  task list                  list all tasks + their status/assignee
  task get <id>              show one task
  task claim <id>            claim a task for this pane (deps must be done)
  task next [--start] [--agent <cmd>]   claim the next ready task (--start spawns
                             an isolated worker); for an agent loop draining the queue
  task start <id> [--branch <b>] [--agent <cmd>]   spawn an isolated worker:
                             a git worktree + pane, auto-claimed and path-leased
  task heartbeat <id> --context <0..1>   report context usage (blocks done at >85%)
  task update <id> [--status <s>] [--output <o>] [--note <n>]
  task done <id>             mark done + release its leases
  task merge <id>            integrate the task's branch into bohay/integration
                             (isolated worktree; conflicts block the task)
  task release <id>          return a claimed task to the queue
  lease acquire <glob>... --task <id>   reserve file paths (denied if they overlap)
  lease release <id>         release a lease
  lease list                 list active path leases

events:
  events                     stream live status changes

remote:
  --remote <host> [ssh args] attach to a bohay session on <host> over plain ssh

server:
  server status              is the server running, and what version
  server start               start the background server if it isn't up
  server stop                stop the server (and all panes)
  server restart             stop + start (load a newly-installed binary)
  integration install|uninstall <claude|copilot|codex|opencode>
                             add/remove bohay's session-resume hook (uninstall
                             removes only bohay's hook, never the agent)
";

pub fn run(args: &[String]) -> Result<i32> {
    if args.get(1).map(String::as_str) == Some("help") {
        print!("{USAGE}");
        return Ok(0);
    }
    // `doctor` is a local environment check — no server needed.
    if args.get(1).map(String::as_str) == Some("doctor") {
        return Ok(doctor());
    }
    // `module install` clones + builds locally (with a confirm prompt), then
    // registers over the socket — it isn't a plain request/response.
    if args.get(1).map(String::as_str) == Some("module")
        && args.get(2).map(String::as_str) == Some("install")
    {
        return module_install(args);
    }
    // `module search` is a read-only GitHub lookup — no server involved.
    if args.get(1).map(String::as_str) == Some("module")
        && args.get(2).map(String::as_str) == Some("search")
    {
        return module_search(args);
    }
    // `wait` (docs/18 WA-1) is a client-side poll/stream loop, not a one-shot
    // request — it exits 0 on the condition, 2 on timeout.
    if args.get(1).map(String::as_str) == Some("wait") {
        return wait_cmd(args);
    }
    let (method, params) = parse(args)?;
    let path = crate::persist::socket_path();
    let mut stream = crate::ipc::transport::connect(&path)
        .map_err(|_| anyhow!("no bohay server running (socket: {})", path.display()))?;

    let req = json!({ "id": "1", "method": method, "params": params });
    writeln!(stream, "{req}")?;

    let mut reader = BufReader::new(stream);
    if method == "events.subscribe" {
        // Stream events until the connection closes.
        for line in reader.lines() {
            match line {
                Ok(l) => println!("{l}"),
                Err(_) => break,
            }
        }
        return Ok(0);
    }

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let line = line.trim();
    // Pretty-print and set exit code on error.
    match serde_json::from_str::<Value>(line) {
        Ok(v) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| line.to_string())
            );
            if v.get("error").is_some() {
                return Ok(1);
            }
        }
        Err(_) => println!("{line}"),
    }
    Ok(0)
}

/// `bohay module install owner/repo[/sub] [--ref REF] [--yes]` — clone + build
/// locally, then register over the socket (or directly if the server is down).
fn module_install(args: &[String]) -> Result<i32> {
    let spec = args
        .get(3)
        .filter(|s| !s.starts_with("--"))
        .ok_or_else(|| {
            anyhow!("usage: bohay module install owner/repo[/sub] [--ref REF] [--yes]")
        })?;
    let git_ref = flag(args, "--ref");
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");

    let installed = crate::module::install::install(spec, git_ref.as_deref(), yes)?;
    let params = json!({
        "path": installed.root.display().to_string(),
        "source": installed.source,
    });
    match send_request("module.link", params) {
        Ok(v) if v.get("error").is_some() => {
            // e.g. already registered — leave the checkout but report it.
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            Ok(1)
        }
        Ok(_) => {
            println!("installed {} ({})", installed.id, installed.source);
            Ok(0)
        }
        Err(_) => {
            // Server down: write the registry directly; it loads on next start.
            register_directly(&installed)?;
            println!(
                "installed {} ({}) — start bohay to use it",
                installed.id, installed.source
            );
            Ok(0)
        }
    }
}

/// `bohay doctor` — report which optional external tools are present. The core
/// multiplexer needs none of them; this just tells a fresh install (esp. via
/// `cargo install`, which can't pull in system tools) what each missing tool
/// would unlock and how to get it. Always exits 0 — nothing here is fatal.
fn doctor() -> i32 {
    use std::process::Command;
    // Run `<cmd> <arg>` and return its first non-empty version line, if it runs.
    let probe = |cmd: &str, arg: &str| -> Option<String> {
        let out = Command::new(cmd).arg(arg).output().ok()?;
        let bytes = if !out.stdout.is_empty() {
            out.stdout
        } else {
            out.stderr // ssh prints its version to stderr
        };
        String::from_utf8_lossy(&bytes)
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
    };

    println!("bohay {}\n", env!("CARGO_PKG_VERSION"));
    println!("  ✓ core    the multiplexer (panes · tabs · agents) needs no external tools\n");

    // (name, cmd, version-arg, what it unlocks, required?, install hint)
    let tools = [
        (
            "git",
            "git",
            "--version",
            "git tab · worktrees",
            true,
            "https://git-scm.com  (brew install git)",
        ),
        (
            "gh",
            "gh",
            "--version",
            "GitHub PRs & issues",
            false,
            "https://cli.github.com  (brew install gh)",
        ),
        (
            "ssh",
            "ssh",
            "-V",
            "bohay --remote",
            false,
            "preinstalled on macOS/Linux",
        ),
        (
            "curl",
            "curl",
            "--version",
            "bohay module search",
            false,
            "preinstalled on macOS/Linux",
        ),
    ];

    let mut missing_git = false;
    for (name, cmd, arg, unlocks, required, hint) in tools {
        match probe(cmd, arg) {
            Some(ver) => {
                // Trim noisy version banners (e.g. curl's) to keep it scannable.
                let short: String = ver.chars().take(26).collect();
                println!("  ✓ {name:<6}{short:<28}{unlocks}");
            }
            None => {
                if required {
                    missing_git = true;
                }
                let kind = if required { "needed for" } else { "optional -" };
                println!("  ✗ {name:<6}not found · {kind} {unlocks}");
                println!("           ↳ {hint}");
            }
        }
    }

    println!();
    if missing_git {
        println!("Tip: install `git` to use the git tab & worktrees. Everything else works now.");
    } else {
        println!("All set — you're good to go. ✓");
    }
    0
}

/// `bohay module search [<query>]` — list modules published to the
/// `bohay-module` GitHub topic. Read-only; doesn't need a running server.
fn module_search(args: &[String]) -> Result<i32> {
    let terms: Vec<&str> = args
        .get(3..)
        .unwrap_or(&[])
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();
    let query = (!terms.is_empty()).then(|| terms.join(" "));

    let hits = crate::module::discovery::search(query.as_deref())?;
    if hits.is_empty() {
        println!("No modules found in the `bohay-module` topic yet.");
        println!("Publish one by tagging a public repo with the `bohay-module` topic.");
        return Ok(0);
    }
    for h in &hits {
        println!("  ★ {:<5} {}", h.stars, h.full_name);
        if !h.description.is_empty() {
            println!("          {}", h.description);
        }
        if !h.url.is_empty() {
            println!("          {}", h.url);
        }
    }
    println!(
        "\n{} result(s). Install with:  bohay module install <owner>/<repo>",
        hits.len()
    );
    Ok(0)
}

/// What a `bohay wait …` invocation is waiting for (parsed from argv).
#[derive(Debug, PartialEq)]
enum WaitFor {
    /// `wait output <id> --match <text>`: the pane's recent output contains `text`.
    Output { needle: String },
    /// `wait agent-status <id> --status <s>`: the pane's agent reaches `status`.
    AgentStatus { status: String },
}

#[derive(Debug, PartialEq)]
struct WaitSpec {
    pane: String,
    condition: WaitFor,
    timeout: Option<f64>,
}

/// Parse `bohay wait output|agent-status <id> …` into a [`WaitSpec`] (pure, so
/// it's unit-tested). The pane id is the first numeric positional, else
/// `$BOHAY_PANE_ID`.
fn parse_wait(args: &[String]) -> Result<WaitSpec> {
    let kind = args.get(2).map(String::as_str).unwrap_or("");
    let pane = args
        .get(3)
        .filter(|s| s.parse::<u32>().is_ok())
        .cloned()
        .or_else(|| std::env::var("BOHAY_PANE_ID").ok())
        .ok_or_else(|| anyhow!("wait needs a pane id (or $BOHAY_PANE_ID)"))?;
    let timeout = flag(args, "--timeout").and_then(|s| s.parse::<f64>().ok());
    let condition = match kind {
        "output" => WaitFor::Output {
            needle: flag(args, "--match").ok_or_else(|| {
                anyhow!("usage: bohay wait output <id> --match <text> [--timeout <s>]")
            })?,
        },
        "agent-status" => WaitFor::AgentStatus {
            status: flag(args, "--status").ok_or_else(|| {
                anyhow!("usage: bohay wait agent-status <id> --status done|blocked|working|idle [--timeout <s>]")
            })?,
        },
        _ => return Err(anyhow!("usage: bohay wait output|agent-status <id> …")),
    };
    Ok(WaitSpec {
        pane,
        condition,
        timeout,
    })
}

/// `bohay wait …` — block until the condition holds (exit 0) or the timeout
/// elapses (exit 2). Built entirely on existing API methods + the event stream.
fn wait_cmd(args: &[String]) -> Result<i32> {
    let spec = parse_wait(args)?;
    let deadline = spec
        .timeout
        .map(|t| Instant::now() + Duration::from_secs_f64(t));
    match spec.condition {
        WaitFor::Output { needle } => loop {
            let v = send_request("pane.read", json!({ "pane": spec.pane }))?;
            let text = v
                .get("result")
                .and_then(|r| r.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            if text.contains(&needle) {
                return Ok(0);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Ok(2);
            }
            std::thread::sleep(Duration::from_millis(200));
        },
        WaitFor::AgentStatus { status } => wait_status_stream(&spec.pane, &status, deadline),
    }
}

/// Current agent status of `pane` (global lookup via `pane.status`).
fn pane_status(pane: &str) -> Result<Option<String>> {
    let v = send_request("pane.status", json!({ "pane": pane }))?;
    Ok(v.get("result")
        .and_then(|r| r.get("status"))
        .and_then(|x| x.as_str())
        .map(String::from))
}

/// Block until `pane`'s agent reaches `target` (exit 0), or `deadline` passes
/// (exit 2). Subscribes to the event stream **first**, then polls the current
/// status — so a transition that happens between the poll and the subscribe is
/// never missed (it's already buffered on the stream).
fn wait_status_stream(pane: &str, target: &str, deadline: Option<Instant>) -> Result<i32> {
    let path = crate::persist::socket_path();
    let stream =
        crate::ipc::transport::connect(&path).map_err(|_| anyhow!("no bohay server running"))?;
    let mut writer = stream.clone();
    writeln!(
        writer,
        "{}",
        json!({"id":"1","method":"events.subscribe","params":{}})
    )?;
    let reader = BufReader::new(stream);

    let (tx, rx) = std::sync::mpsc::channel();
    let (pane_s, target_s) = (pane.to_string(), target.to_string());
    std::thread::spawn(move || {
        for line in reader.lines() {
            let Ok(l) = line else { break };
            if let Ok(v) = serde_json::from_str::<Value>(&l) {
                let is_status =
                    v.get("event").and_then(|e| e.as_str()) == Some("pane.agent_status_changed");
                let data = v.get("data");
                let p = data.and_then(|d| d.get("pane")).and_then(|x| x.as_str());
                let s = data.and_then(|d| d.get("status")).and_then(|x| x.as_str());
                if is_status && p == Some(pane_s.as_str()) && s == Some(target_s.as_str()) {
                    let _ = tx.send(());
                    break;
                }
            }
        }
    });

    // Now that we're listening, an initial poll handles the already-there case.
    if pane_status(pane)?.as_deref() == Some(target) {
        return Ok(0);
    }

    match deadline {
        Some(d) => {
            let now = Instant::now();
            if d <= now {
                return Ok(2);
            }
            match rx.recv_timeout(d - now) {
                Ok(()) => Ok(0),
                Err(_) => Ok(2),
            }
        }
        None => {
            let _ = rx.recv();
            Ok(0)
        }
    }
}

/// Focus + zoom a pane via `attach.pane` (docs/18 WA-2). Used by `bohay attach`.
pub fn request_attach(pane: &str) -> Result<()> {
    send_request("attach.pane", json!({ "pane": pane })).map(|_| ())
}

/// One request/response over the control socket.
fn send_request(method: &str, params: Value) -> Result<Value> {
    let path = crate::persist::socket_path();
    let mut stream =
        crate::ipc::transport::connect(&path).map_err(|_| anyhow!("no bohay server running"))?;
    let req = json!({ "id": "1", "method": method, "params": params });
    writeln!(stream, "{req}")?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    serde_json::from_str(line.trim()).map_err(|e| anyhow!("bad reply: {e}"))
}

/// Register an installed module by writing the registry file directly (used when
/// no server is running).
fn register_directly(installed: &crate::module::install::Installed) -> Result<()> {
    use crate::module::{manifest::ModuleManifest, registry, InstalledModule};
    let mut reg = registry::load();
    reg.modules.retain(|m| m.id != installed.id);
    let manifest = ModuleManifest::load(&installed.root).map_err(|e| anyhow!(e))?;
    reg.modules.push(InstalledModule {
        id: installed.id.clone(),
        root: installed.root.clone(),
        enabled: true,
        source: Some(installed.source.clone()),
        manifest,
        warning: None,
    });
    registry::save(&reg);
    Ok(())
}

/// Map argv to a `(method, params)` pair.
fn parse(args: &[String]) -> Result<(String, Value)> {
    let noun = args.get(1).map(String::as_str).unwrap_or("");
    let verb = args.get(2).map(String::as_str).unwrap_or("");
    let rest = &args[3.min(args.len())..];

    // The pane id is the first numeric positional, else $BOHAY_PANE_ID.
    let pane = || -> Value {
        if let Some(first) = rest.first() {
            if first.parse::<u32>().is_ok() {
                return json!(first);
            }
        }
        match std::env::var("BOHAY_PANE_ID") {
            Ok(v) => json!(v),
            Err(_) => Value::Null,
        }
    };
    // Args after an optional leading numeric pane id.
    let tail = || -> Vec<String> {
        let skip = rest
            .first()
            .map(|s| s.parse::<u32>().is_ok())
            .unwrap_or(false);
        rest[if skip { 1 } else { 0 }..].to_vec()
    };

    let with_pane = |mut obj: serde_json::Map<String, Value>| {
        let p = pane();
        if !p.is_null() {
            obj.insert("pane".to_string(), p);
        }
        Value::Object(obj)
    };

    // First positional arg after the verb (for workspace/tab indices).
    let arg0 = || rest.first().cloned();
    let one = |key: &str, val: Option<String>| {
        let mut obj = serde_json::Map::new();
        if let Some(v) = val {
            obj.insert(key.to_string(), json!(v));
        }
        Value::Object(obj)
    };

    Ok(match (noun, verb) {
        ("ping", _) => ("ping".into(), json!({})),
        ("events", _) => ("events.subscribe".into(), json!({})),
        ("agent", "sessions") => ("agent.sessions".into(), json!({})),
        ("agent", "resume") => ("agent.resume".into(), one("session_id", arg0())),
        ("agent", _) => ("agent.list".into(), json!({})),

        ("ui", "sidebar") => {
            let mut obj = serde_json::Map::new();
            if let Some(s) = flag(args, "--side") {
                obj.insert("side".to_string(), json!(s));
            }
            if let Some(w) = flag(args, "--width") {
                obj.insert("width".to_string(), json!(w));
            }
            if args.iter().any(|a| a == "--hide") {
                obj.insert("visible".to_string(), json!(false));
            } else if args.iter().any(|a| a == "--show") {
                obj.insert("visible".to_string(), json!(true));
            }
            ("ui.sidebar".into(), Value::Object(obj))
        }

        // Sidebar docks for module/plugin authors (docs/29). `push` feeds rows to
        // a dock: `--rows '<json array>'`, or pipe the JSON array on stdin.
        ("ui", "dock") => {
            let sub = rest.first().map(String::as_str).unwrap_or("list");
            match sub {
                "push" => {
                    let mut obj = serde_json::Map::new();
                    let id = flag(args, "--id")
                        .ok_or_else(|| anyhow!("usage: bohay ui dock push --id <id> [--title <t>] [--side left|right] [--rows <json>]"))?;
                    obj.insert("id".to_string(), json!(id));
                    if let Some(tt) = flag(args, "--title") {
                        obj.insert("title".to_string(), json!(tt));
                    }
                    if let Some(side) = flag(args, "--side") {
                        obj.insert("placement".to_string(), json!(side));
                    }
                    let rows_str = match flag(args, "--rows") {
                        Some(s) => s,
                        None => {
                            use std::io::Read;
                            let mut s = String::new();
                            let _ = std::io::stdin().read_to_string(&mut s);
                            s
                        }
                    };
                    let rows: Value = if rows_str.trim().is_empty() {
                        json!([])
                    } else {
                        serde_json::from_str(&rows_str)
                            .map_err(|e| anyhow!("--rows must be a JSON array: {e}"))?
                    };
                    obj.insert("rows".to_string(), rows);
                    ("ui.dock.push".into(), Value::Object(obj))
                }
                "move" => {
                    let mut obj = serde_json::Map::new();
                    if let Some(id) = flag(args, "--id") {
                        obj.insert("id".to_string(), json!(id));
                    }
                    if let Some(side) = flag(args, "--side") {
                        obj.insert("side".to_string(), json!(side));
                    }
                    ("ui.dock.move".into(), Value::Object(obj))
                }
                _ => ("ui.dock.list".into(), json!({})),
            }
        }

        ("workspace" | "node", "new") => ("workspace.new".into(), json!({})),
        ("workspace" | "node", "open") => ("workspace.open".into(), one("path", arg0())),
        ("workspace" | "node", "focus") => ("workspace.focus".into(), one("workspace", arg0())),
        ("workspace" | "node", "close") => ("workspace.close".into(), one("workspace", arg0())),
        ("workspace" | "node", _) => ("workspace.list".into(), json!({})),

        ("tab", "new") => ("tab.new".into(), json!({})),
        ("tab", "focus") => ("tab.focus".into(), one("tab", arg0())),
        ("tab", "close") => ("tab.close".into(), one("tab", arg0())),
        ("tab", _) => ("tab.list".into(), json!({})),

        ("pane", "split") => {
            let mut obj = serde_json::Map::new();
            if args.iter().any(|a| a == "--down" || a == "--stack") {
                obj.insert("direction".to_string(), json!("down"));
            }
            ("pane.split".into(), with_pane(obj))
        }
        ("pane", "focus") => ("pane.focus".into(), with_pane(serde_json::Map::new())),
        ("pane", "run") => {
            let command = tail().join(" ");
            let mut obj = serde_json::Map::new();
            obj.insert("command".to_string(), json!(command));
            ("pane.run".into(), with_pane(obj))
        }
        ("pane", "send") => {
            let text = tail().join(" ");
            let mut obj = serde_json::Map::new();
            obj.insert("text".to_string(), json!(text));
            ("pane.send_input".into(), with_pane(obj))
        }
        ("pane", "read") => ("pane.read".into(), with_pane(serde_json::Map::new())),
        ("pane", "status") => ("pane.status".into(), with_pane(serde_json::Map::new())),
        ("pane", "close") => ("pane.close".into(), with_pane(serde_json::Map::new())),
        ("pane", "report") => {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "agent".to_string(),
                json!(flag(args, "--agent").unwrap_or_default()),
            );
            obj.insert(
                "session_id".to_string(),
                json!(flag(args, "--session").unwrap_or_default()),
            );
            ("pane.report_session".into(), with_pane(obj))
        }
        ("pane", "report-event") => {
            let mut obj = serde_json::Map::new();
            obj.insert(
                "agent".to_string(),
                json!(flag(args, "--agent").unwrap_or_default()),
            );
            obj.insert(
                "kind".to_string(),
                json!(flag(args, "--kind").unwrap_or_default()),
            );
            obj.insert(
                "message".to_string(),
                json!(flag(args, "--message").unwrap_or_default()),
            );
            if let Some(t) = flag(args, "--tool") {
                obj.insert("tool".to_string(), json!(t));
            }
            ("pane.report_event".into(), with_pane(obj))
        }
        ("pane", _) => ("pane.list".into(), json!({})),

        ("module", "link") => {
            let mut obj = serde_json::Map::new();
            if let Some(path) = rest.first() {
                obj.insert("path".to_string(), json!(path));
            }
            if args.iter().any(|a| a == "--disabled") {
                obj.insert("disabled".to_string(), json!(true));
            }
            ("module.link".into(), Value::Object(obj))
        }
        ("module", "unlink") => ("module.unlink".into(), one("id", arg0())),
        ("module", "uninstall") => ("module.uninstall".into(), one("id", arg0())),
        ("module", "enable") => ("module.enable".into(), one("id", arg0())),
        ("module", "disable") => ("module.disable".into(), one("id", arg0())),
        ("module", "run") => {
            let mut obj = serde_json::Map::new();
            match (rest.first(), rest.get(1)) {
                (Some(m), Some(a)) => {
                    obj.insert("module".to_string(), json!(m));
                    obj.insert("id".to_string(), json!(a));
                }
                (Some(a), None) => {
                    obj.insert("id".to_string(), json!(a));
                }
                _ => return Err(anyhow!("usage: bohay module run <module> <action>")),
            }
            ("module.action.invoke".into(), Value::Object(obj))
        }
        ("module", "actions") => ("module.action.list".into(), json!({})),
        ("module", "log") => {
            let mut obj = serde_json::Map::new();
            if let Some(id) = rest.first().filter(|s| !s.starts_with("--")) {
                obj.insert("id".to_string(), json!(id));
            }
            if let Some(n) = flag(args, "--limit").and_then(|s| s.parse::<u64>().ok()) {
                obj.insert("limit".to_string(), json!(n));
            }
            ("module.log.list".into(), Value::Object(obj))
        }
        ("module", "info") => ("module.info".into(), one("id", arg0())),
        ("module", "config-dir") => ("module.config_dir".into(), one("id", arg0())),
        ("module", "pane") => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            match sub {
                "open" => {
                    let mut obj = serde_json::Map::new();
                    if let Some(m) = rest.get(1) {
                        obj.insert("module".to_string(), json!(m));
                    }
                    if let Some(e) = rest.get(2) {
                        obj.insert("entrypoint".to_string(), json!(e));
                    }
                    if let Some(pl) = flag(args, "--placement") {
                        obj.insert("placement".to_string(), json!(pl));
                    }
                    ("module.pane.open".into(), Value::Object(obj))
                }
                "focus" => (
                    "module.pane.focus".into(),
                    one("pane", rest.get(1).cloned()),
                ),
                "close" => (
                    "module.pane.close".into(),
                    one("pane", rest.get(1).cloned()),
                ),
                _ => return Err(anyhow!("usage: bohay module pane open|focus|close …")),
            }
        }
        ("module", _) => ("module.list".into(), json!({})),

        ("git", "branches") => ("git.branches".into(), json!({})),
        ("git", "log") => {
            let mut obj = serde_json::Map::new();
            if let Some(n) = flag(args, "--limit").and_then(|s| s.parse::<u64>().ok()) {
                obj.insert("n".to_string(), json!(n));
            }
            ("git.log".into(), Value::Object(obj))
        }
        ("git", "open") => ("git.open".into(), one("workspace", arg0())),
        ("git", _) => ("git.status".into(), json!({})),

        ("worktree", "create") => ("worktree.create".into(), one("branch", arg0())),
        ("worktree", "open") => ("worktree.open".into(), one("path", arg0())),
        ("worktree", "remove") => ("worktree.remove".into(), one("path", arg0())),
        ("worktree", _) => ("worktree.list".into(), json!({})),

        // ── orchestration (docs/22, M0): task ledger + path leases ──────────
        ("task", "add") => {
            let title = rest.iter().find(|a| !a.starts_with("--")).cloned();
            let mut obj = serde_json::Map::new();
            obj.insert("title".into(), json!(title.unwrap_or_default()));
            obj.insert("paths".into(), json!(multi_flag(args, "--paths")));
            obj.insert("deps".into(), json!(multi_flag(args, "--dep")));
            if let Some(g) = flag(args, "--gate") {
                obj.insert("gate".into(), json!(g));
            }
            ("task.add".into(), Value::Object(obj))
        }
        ("task", "get") => ("task.get".into(), one("id", arg0())),
        ("task", "next") => {
            let mut obj = serde_json::Map::new();
            if args.iter().any(|a| a == "--start") {
                obj.insert("start".into(), json!(true));
            }
            if let Some(a) = flag(args, "--agent") {
                obj.insert("agent".into(), json!(a));
            }
            let pv = pane();
            if !pv.is_null() {
                obj.insert("pane".into(), pv);
            }
            ("task.next".into(), Value::Object(obj))
        }
        ("task", "heartbeat") => {
            let mut obj = serde_json::Map::new();
            if let Some(id) = arg0() {
                obj.insert("id".into(), json!(id));
            }
            if let Some(c) = flag(args, "--context").and_then(|s| s.parse::<f64>().ok()) {
                obj.insert("context".into(), json!(c));
            }
            ("task.heartbeat".into(), Value::Object(obj))
        }
        ("task", "start") => {
            let mut obj = serde_json::Map::new();
            if let Some(id) = arg0() {
                obj.insert("id".into(), json!(id));
            }
            if let Some(b) = flag(args, "--branch") {
                obj.insert("branch".into(), json!(b));
            }
            if let Some(a) = flag(args, "--agent") {
                obj.insert("agent".into(), json!(a));
            }
            ("task.start".into(), Value::Object(obj))
        }
        ("task", "claim") => {
            let mut obj = serde_json::Map::new();
            if let Some(id) = arg0() {
                obj.insert("id".into(), json!(id));
            }
            let pv = pane();
            if !pv.is_null() {
                obj.insert("pane".into(), pv);
            }
            ("task.claim".into(), Value::Object(obj))
        }
        ("task", "done") => ("task.done".into(), one("id", arg0())),
        ("task", "merge") => ("task.merge".into(), one("id", arg0())),
        ("task", "release") => ("task.release".into(), one("id", arg0())),
        ("task", "update") => {
            let mut obj = serde_json::Map::new();
            if let Some(id) = arg0() {
                obj.insert("id".into(), json!(id));
            }
            if let Some(s) = flag(args, "--status") {
                obj.insert("status".into(), json!(s));
            }
            if let Some(o) = flag(args, "--output") {
                obj.insert("output".into(), json!(o));
            }
            if let Some(n) = flag(args, "--note") {
                obj.insert("note".into(), json!(n));
            }
            ("task.update".into(), Value::Object(obj))
        }
        ("task", _) => ("task.list".into(), json!({})),

        ("lease", "acquire") => {
            // Positional paths up to the first flag, plus `--task <id>`.
            let paths: Vec<String> = rest
                .iter()
                .take_while(|a| !a.starts_with("--"))
                .cloned()
                .collect();
            let mut obj = serde_json::Map::new();
            obj.insert("paths".into(), json!(paths));
            if let Some(t) = flag(args, "--task") {
                obj.insert("task".into(), json!(t));
            }
            let pv = pane();
            if !pv.is_null() {
                obj.insert("pane".into(), pv);
            }
            ("lease.acquire".into(), Value::Object(obj))
        }
        ("lease", "release") => ("lease.release".into(), one("id", arg0())),
        ("lease", _) => ("lease.list".into(), json!({})),

        _ => return Err(anyhow!("unknown command. Try `bohay help`.")),
    })
}

/// Collect values that follow every occurrence of `--name` up to the next flag,
/// e.g. `--paths a b c` → `[a,b,c]`, and repeated `--dep t1 --dep t2` → `[t1,t2]`.
fn multi_flag(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            let mut j = i + 1;
            while j < args.len() && !args[j].starts_with("--") {
                out.push(args[j].clone());
                j += 1;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Value following `--name` in argv, if present.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn maps_commands() {
        std::env::remove_var("BOHAY_PANE_ID");
        let (m, _) = parse(&argv("bohay ping")).unwrap();
        assert_eq!(m, "ping");

        let (m, _) = parse(&argv("bohay pane list")).unwrap();
        assert_eq!(m, "pane.list");

        let (m, p) = parse(&argv("bohay pane split --down")).unwrap();
        assert_eq!(m, "pane.split");
        assert_eq!(p.get("direction").and_then(|v| v.as_str()), Some("down"));

        let (m, p) = parse(&argv("bohay pane run 3 echo hi")).unwrap();
        assert_eq!(m, "pane.run");
        assert_eq!(p.get("pane").and_then(|v| v.as_str()), Some("3"));
        assert_eq!(p.get("command").and_then(|v| v.as_str()), Some("echo hi"));

        let (m, _) = parse(&argv("bohay node list")).unwrap();
        assert_eq!(m, "workspace.list");
        let (m, p) = parse(&argv("bohay node focus 2")).unwrap();
        assert_eq!(m, "workspace.focus");
        assert_eq!(p.get("workspace").and_then(|v| v.as_str()), Some("2"));
        let (m, _) = parse(&argv("bohay tab new")).unwrap();
        assert_eq!(m, "tab.new");
        let (m, _) = parse(&argv("bohay agent list")).unwrap();
        assert_eq!(m, "agent.list");
    }

    #[test]
    fn maps_sidebar_dock_commands() {
        std::env::remove_var("BOHAY_PANE_ID");

        // A dock push with inline rows (docs/29) — the plugin API.
        let av: Vec<String> = vec![
            "bohay".into(),
            "ui".into(),
            "dock".into(),
            "push".into(),
            "--id".into(),
            "you.ci".into(),
            "--title".into(),
            "CI".into(),
            "--side".into(),
            "right".into(),
            "--rows".into(),
            r#"[{"text":"build ok","dot":"done","action":"open"}]"#.into(),
        ];
        let (m, p) = parse(&av).unwrap();
        assert_eq!(m, "ui.dock.push");
        assert_eq!(p.get("id").and_then(|v| v.as_str()), Some("you.ci"));
        assert_eq!(p.get("title").and_then(|v| v.as_str()), Some("CI"));
        assert_eq!(p.get("placement").and_then(|v| v.as_str()), Some("right"));
        let rows = p.get("rows").and_then(|v| v.as_array()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("text").and_then(|v| v.as_str()),
            Some("build ok")
        );

        // Missing --id is a clear error.
        assert!(parse(&argv("bohay ui dock push")).is_err());

        let (m, p) = parse(&argv("bohay ui dock move --id you.ci --side left")).unwrap();
        assert_eq!(m, "ui.dock.move");
        assert_eq!(p.get("side").and_then(|v| v.as_str()), Some("left"));

        let (m, _) = parse(&argv("bohay ui dock list")).unwrap();
        assert_eq!(m, "ui.dock.list");

        // `ui sidebar` now takes an optional side.
        let (m, p) = parse(&argv("bohay ui sidebar --side right --width 30")).unwrap();
        assert_eq!(m, "ui.sidebar");
        assert_eq!(p.get("side").and_then(|v| v.as_str()), Some("right"));
    }

    #[test]
    fn maps_orchestration_commands() {
        std::env::remove_var("BOHAY_PANE_ID");

        let (m, p) = parse(&argv(
            "bohay task add title --paths src/auth src/api --dep t1 --gate cargo",
        ))
        .unwrap();
        assert_eq!(m, "task.add");
        assert_eq!(p.get("title").and_then(|v| v.as_str()), Some("title"));
        assert_eq!(
            p.get("paths").and_then(|v| v.as_array()).map(|a| a.len()),
            Some(2)
        );
        assert_eq!(
            p.get("deps").and_then(|v| v.as_array()).map(|a| a.len()),
            Some(1)
        );
        assert_eq!(p.get("gate").and_then(|v| v.as_str()), Some("cargo"));

        let (m, _) = parse(&argv("bohay task list")).unwrap();
        assert_eq!(m, "task.list");
        let (m, p) = parse(&argv("bohay task claim t3")).unwrap();
        assert_eq!(m, "task.claim");
        assert_eq!(p.get("id").and_then(|v| v.as_str()), Some("t3"));
        let (m, _) = parse(&argv("bohay task done t3")).unwrap();
        assert_eq!(m, "task.done");

        let (m, p) = parse(&argv("bohay lease acquire src/auth/** --task t1")).unwrap();
        assert_eq!(m, "lease.acquire");
        assert_eq!(
            p.get("paths").and_then(|v| v.as_array()).map(|a| a.len()),
            Some(1)
        );
        assert_eq!(p.get("task").and_then(|v| v.as_str()), Some("t1"));

        // ORCH-3/4/5/6 verbs.
        let (m, p) = parse(&argv("bohay task start t1 --branch feat --agent claude")).unwrap();
        assert_eq!(m, "task.start");
        assert_eq!(p.get("branch").and_then(|v| v.as_str()), Some("feat"));
        assert_eq!(p.get("agent").and_then(|v| v.as_str()), Some("claude"));

        let (m, p) = parse(&argv("bohay task next --start --agent claude")).unwrap();
        assert_eq!(m, "task.next");
        assert_eq!(p.get("start").and_then(|v| v.as_bool()), Some(true));

        let (m, p) = parse(&argv("bohay task heartbeat t1 --context 0.7")).unwrap();
        assert_eq!(m, "task.heartbeat");
        assert_eq!(p.get("context").and_then(|v| v.as_f64()), Some(0.7));

        let (m, p) = parse(&argv("bohay task merge t1")).unwrap();
        assert_eq!(m, "task.merge");
        assert_eq!(p.get("id").and_then(|v| v.as_str()), Some("t1"));
    }

    #[test]
    fn maps_module_commands() {
        let (m, _) = parse(&argv("bohay module list")).unwrap();
        assert_eq!(m, "module.list");

        let (m, p) = parse(&argv("bohay module link ./mod --disabled")).unwrap();
        assert_eq!(m, "module.link");
        assert_eq!(p.get("path").and_then(|v| v.as_str()), Some("./mod"));
        assert_eq!(p.get("disabled").and_then(|v| v.as_bool()), Some(true));

        let (m, p) = parse(&argv("bohay module run my-mod refresh")).unwrap();
        assert_eq!(m, "module.action.invoke");
        assert_eq!(p.get("module").and_then(|v| v.as_str()), Some("my-mod"));
        assert_eq!(p.get("id").and_then(|v| v.as_str()), Some("refresh"));

        let (m, p) = parse(&argv("bohay module run refresh")).unwrap();
        assert_eq!(m, "module.action.invoke");
        assert_eq!(p.get("id").and_then(|v| v.as_str()), Some("refresh"));
        assert!(p.get("module").is_none());

        let (m, p) = parse(&argv("bohay module enable my-mod")).unwrap();
        assert_eq!(m, "module.enable");
        assert_eq!(p.get("id").and_then(|v| v.as_str()), Some("my-mod"));
    }

    #[test]
    fn parses_wait() {
        std::env::remove_var("BOHAY_PANE_ID");
        let s = parse_wait(&argv("bohay wait output 3 --match done --timeout 5")).unwrap();
        assert_eq!(s.pane, "3");
        assert_eq!(s.timeout, Some(5.0));
        assert_eq!(
            s.condition,
            WaitFor::Output {
                needle: "done".into()
            }
        );

        let s = parse_wait(&argv("bohay wait agent-status 7 --status blocked")).unwrap();
        assert_eq!(s.pane, "7");
        assert_eq!(s.timeout, None);
        assert_eq!(
            s.condition,
            WaitFor::AgentStatus {
                status: "blocked".into()
            }
        );

        // missing --match is an error
        assert!(parse_wait(&argv("bohay wait output 3")).is_err());
        // pane id falls back to $BOHAY_PANE_ID
        std::env::set_var("BOHAY_PANE_ID", "9");
        let s = parse_wait(&argv("bohay wait output --match hi")).unwrap();
        assert_eq!(s.pane, "9");
        std::env::remove_var("BOHAY_PANE_ID");
    }

    #[test]
    fn maps_git_commands() {
        let (m, _) = parse(&argv("bohay git status")).unwrap();
        assert_eq!(m, "git.status");
        let (m, _) = parse(&argv("bohay git")).unwrap();
        assert_eq!(m, "git.status");
        let (m, _) = parse(&argv("bohay git branches")).unwrap();
        assert_eq!(m, "git.branches");
        let (m, p) = parse(&argv("bohay git log --limit 5")).unwrap();
        assert_eq!(m, "git.log");
        assert_eq!(p.get("n").and_then(|v| v.as_u64()), Some(5));
        let (m, p) = parse(&argv("bohay git open 2")).unwrap();
        assert_eq!(m, "git.open");
        assert_eq!(p.get("workspace").and_then(|v| v.as_str()), Some("2"));
    }

    #[test]
    fn maps_worktree_commands() {
        let (m, _) = parse(&argv("bohay worktree list")).unwrap();
        assert_eq!(m, "worktree.list");
        let (m, p) = parse(&argv("bohay worktree create feature/x")).unwrap();
        assert_eq!(m, "worktree.create");
        assert_eq!(p.get("branch").and_then(|v| v.as_str()), Some("feature/x"));
        let (m, p) = parse(&argv("bohay worktree open /tmp/wt")).unwrap();
        assert_eq!(m, "worktree.open");
        assert_eq!(p.get("path").and_then(|v| v.as_str()), Some("/tmp/wt"));
        let (m, p) = parse(&argv("bohay worktree remove /tmp/wt")).unwrap();
        assert_eq!(m, "worktree.remove");
        assert_eq!(p.get("path").and_then(|v| v.as_str()), Some("/tmp/wt"));
    }

    // The docs site's CLI reference (website/…/reference/cli.mdx) carries the
    // `bohay help` text VERBATIM — this guard fails CI if a command changes
    // without the docs page being regenerated, so the two can never drift.
    // Regenerate with:  bohay help  →  the page's ```txt block.
    #[test]
    fn docs_cli_reference_matches_help() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("website/src/content/docs/docs/reference/cli.mdx");
        let Ok(page) = std::fs::read_to_string(&path) else {
            return; // published crate / partial checkout — nothing to check
        };
        let block = page
            .split("```txt\n")
            .nth(1)
            .and_then(|rest| rest.split("\n```").next())
            .expect("cli.mdx must contain a ```txt block");
        // USAGE ends with a newline the split consumes; compare trimmed.
        assert_eq!(
            block.trim_end(),
            USAGE.trim_end(),
            "website/…/reference/cli.mdx has drifted from `bohay help` — \
             regenerate the page's txt block from the USAGE text"
        );
    }
}
