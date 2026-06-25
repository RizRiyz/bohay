//! Module registry operations + the action/command runner, driven from the
//! `module.*` socket API (docs/13 MOD-1). Registry edits persist immediately;
//! command runs are fire-and-forget with a `Running` log filled in when the
//! subprocess finishes (`AppEvent::ModuleCommandFinished`).

use std::path::Path;

use super::*;
use crate::module::manifest::ModuleManifest;
use crate::module::runtime::{ModuleCommandLog, ModuleStatus};
use crate::module::{context, paths, registry, runtime, InstalledModule, ModulePaneRecord};

impl App {
    /// Register a module dir, recording its install `source` (a git install
    /// passes `owner/repo@<sha>`; a local link passes `None`).
    pub fn module_link_with(
        &mut self,
        path: &Path,
        enabled: bool,
        source: Option<String>,
    ) -> Result<String, String> {
        let root = path
            .canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", path.display()))?;
        let manifest = ModuleManifest::load(&root)?;
        let id = manifest.id.clone();
        if self.modules.find(&id).is_some() {
            return Err(format!("module {id} is already registered"));
        }
        self.modules.modules.push(InstalledModule {
            id: id.clone(),
            root,
            enabled,
            source,
            manifest,
            warning: None,
        });
        registry::save(&self.modules);
        Ok(id)
    }

    /// Uninstall a git-installed module: remove it from the registry **and**
    /// delete its managed checkout (guarded — refuses for locally-linked modules).
    pub fn module_uninstall(&mut self, id: &str) -> Result<(), String> {
        let root = self
            .modules
            .find(id)
            .map(|m| m.root.clone())
            .ok_or_else(|| format!("no module {id}"))?;
        if !crate::module::install::is_removable(&root) {
            return Err(format!(
                "{id} is a linked module (its files aren't managed by bohay) — use `module unlink`"
            ));
        }
        self.modules.modules.retain(|m| m.id != id);
        registry::save(&self.modules);
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    /// Remove a module from the registry (does not touch its files).
    pub fn module_unlink(&mut self, id: &str) -> Result<(), String> {
        let before = self.modules.modules.len();
        self.modules.modules.retain(|m| m.id != id);
        if self.modules.modules.len() == before {
            return Err(format!("no module {id}"));
        }
        registry::save(&self.modules);
        Ok(())
    }

    pub fn module_set_enabled(&mut self, id: &str, on: bool) -> Result<(), String> {
        let m = self
            .modules
            .find_mut(id)
            .ok_or_else(|| format!("no module {id}"))?;
        m.enabled = on;
        registry::save(&self.modules);
        Ok(())
    }

    /// Ensure (and return) a module's config dir.
    pub fn module_config_dir(&self, id: &str) -> Result<std::path::PathBuf, String> {
        if self.modules.find(id).is_none() {
            return Err(format!("no module {id}"));
        }
        let dir = paths::config_dir(id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        Ok(dir)
    }

    /// Invoke an action by id, optionally constrained to one module. Resolves by
    /// action id when unambiguous, else requires `module`. Returns the log id.
    pub fn module_invoke_action(
        &mut self,
        action_id: &str,
        module_filter: Option<&str>,
        source: &str,
    ) -> Result<u64, String> {
        // When a specific module is named, validate it up front for a clear
        // error (e.g. "disabled") instead of a generic "no runnable module …".
        if let Some(mid) = module_filter {
            match self.modules.find(mid) {
                None => return Err(format!("no module {mid}")),
                Some(m) if !m.is_runnable() => {
                    return Err(m
                        .warning
                        .clone()
                        .unwrap_or_else(|| format!("module {mid} is disabled")))
                }
                Some(m) if m.manifest.action(action_id).is_none() => {
                    return Err(format!("module {mid} has no action {action_id}"))
                }
                _ => {}
            }
        }
        let matches: Vec<(String, Vec<String>)> = self
            .modules
            .modules
            .iter()
            .filter(|m| m.is_runnable())
            .filter(|m| module_filter.is_none_or(|f| m.id == f))
            .filter_map(|m| {
                m.manifest
                    .action(action_id)
                    .map(|a| (m.id.clone(), a.command.clone()))
            })
            .collect();
        let (module_id, argv) = match matches.len() {
            0 => return Err(format!("no runnable module has action {action_id}")),
            1 => matches.into_iter().next().unwrap(),
            _ => {
                return Err(format!(
                    "action {action_id} is ambiguous — pass a module id"
                ))
            }
        };
        let extra = vec![("BOHAY_MODULE_ACTION_ID".to_string(), action_id.to_string())];
        self.run_module_command(
            &module_id,
            argv,
            format!("action:{action_id}"),
            extra,
            source,
        )
    }

    /// Publish a lifecycle event to `events.subscribe` subscribers **and** run
    /// any enabled module's matching `[[events]]` hook (MOD-3). The payload is
    /// passed to hooks as `BOHAY_MODULE_EVENT_JSON`.
    pub fn emit_event(&mut self, name: &str, data: serde_json::Value) {
        let event_json = data.to_string();
        api::publish(
            &self.events,
            json!({ "event": name, "data": data }).to_string(),
        );
        let mut targets: Vec<(String, Vec<String>)> = Vec::new();
        for m in &self.modules.modules {
            if !m.is_runnable() {
                continue;
            }
            for e in &m.manifest.events {
                if e.on == name {
                    targets.push((m.id.clone(), e.command.clone()));
                }
            }
        }
        for (module_id, argv) in targets {
            let extra = vec![
                ("BOHAY_MODULE_EVENT".to_string(), name.to_string()),
                ("BOHAY_MODULE_EVENT_JSON".to_string(), event_json.clone()),
            ];
            let _ =
                self.run_module_command(&module_id, argv, format!("event:{name}"), extra, "event");
        }
    }

    /// Open a module's `[[panes]]` entrypoint as a real bohay pane (MOD-2),
    /// placed per `placement` (split | overlay | tab; default split). Returns the
    /// new pane id.
    pub fn module_open_pane(
        &mut self,
        module_id: &str,
        entrypoint: &str,
        placement: Option<&str>,
        source: &str,
    ) -> Result<PaneId, String> {
        let argv = {
            let m = self
                .modules
                .find(module_id)
                .ok_or_else(|| format!("no module {module_id}"))?;
            if !m.is_runnable() {
                return Err(m
                    .warning
                    .clone()
                    .unwrap_or_else(|| format!("module {module_id} is disabled")));
            }
            m.manifest
                .panes
                .iter()
                .find(|p| p.id == entrypoint)
                .map(|p| p.command.clone())
                .ok_or_else(|| format!("module {module_id} has no pane {entrypoint}"))?
        };
        let placement = placement.unwrap_or("split");

        let ctx = context::build(self, source);
        let (root, mut env) = {
            let m = self.modules.find(module_id).unwrap();
            (m.root.clone(), runtime::base_env(m, &ctx))
        };
        env.push((
            "BOHAY_MODULE_ENTRYPOINT_ID".to_string(),
            entrypoint.to_string(),
        ));

        // The pane runs the argv in the module root (so relative paths resolve);
        // the script reads the node cwd from the context.
        let id = PaneId::alloc();
        let pane = Pane::spawn_command(id, 80, 24, root, self.app_tx.clone(), &argv, &env)
            .map_err(|e| format!("cannot spawn module pane: {e}"))?;
        let cmd = pane.command.clone();
        self.panes.insert(id, pane);
        self.status.insert(id, PaneStatus::new(cmd));
        self.session_dirty = true;

        match placement {
            "tab" => {
                let ws = &mut self.workspaces[self.active_ws];
                ws.tabs.push(Tab::panes(TileLayout::new(id)));
                ws.active_tab = ws.tabs.len() - 1;
                self.zoomed = false;
            }
            "overlay" => {
                self.layout_mut().split_focused(Axis::Col, id);
                self.zoomed = true; // fill the screen, overlay-style
            }
            _ => {
                self.layout_mut().split_focused(Axis::Col, id);
                self.zoomed = false;
            }
        }
        self.module_panes.insert(
            id,
            ModulePaneRecord {
                module_id: module_id.to_string(),
                entrypoint: entrypoint.to_string(),
            },
        );
        self.emit_event(
            "pane.created",
            json!({"pane": id.0.to_string(), "module": module_id}),
        );
        Ok(id)
    }

    /// Run an argv command for a module: build env + context, enforce the
    /// in-flight cap, push a `Running` log, and spawn the subprocess.
    pub fn run_module_command(
        &mut self,
        module_id: &str,
        argv: Vec<String>,
        label: String,
        extra_env: Vec<(String, String)>,
        source: &str,
    ) -> Result<u64, String> {
        {
            let module = self
                .modules
                .find(module_id)
                .ok_or_else(|| format!("no module {module_id}"))?;
            if !module.is_runnable() {
                return Err(module
                    .warning
                    .clone()
                    .unwrap_or_else(|| format!("module {module_id} is disabled")));
            }
        }
        let in_flight = self
            .module_logs
            .iter()
            .filter(|l| l.status == ModuleStatus::Running)
            .count();
        if in_flight >= runtime::MAX_IN_FLIGHT {
            return Err(format!(
                "too many module commands in flight (max {})",
                runtime::MAX_IN_FLIGHT
            ));
        }
        let ctx = context::build(self, source);
        let (root, mut env) = {
            let module = self.modules.find(module_id).unwrap();
            (module.root.clone(), runtime::base_env(module, &ctx))
        };
        env.extend(extra_env);
        let log_id = runtime::next_log_id();
        self.push_module_log(ModuleCommandLog {
            id: log_id,
            module_id: module_id.to_string(),
            label,
            argv: argv.clone(),
            status: ModuleStatus::Running,
            code: None,
            out: String::new(),
            err: String::new(),
        });
        runtime::spawn(log_id, root, argv, env, self.app_tx.clone());
        Ok(log_id)
    }

    fn push_module_log(&mut self, log: ModuleCommandLog) {
        self.module_logs.push(log);
        let n = self.module_logs.len();
        if n > runtime::LOG_LIMIT {
            self.module_logs.drain(0..n - runtime::LOG_LIMIT);
        }
    }

    /// Fill in a command log when its subprocess finishes.
    pub fn module_command_finished(
        &mut self,
        log_id: u64,
        code: Option<i32>,
        out: String,
        err: String,
    ) {
        if let Some(log) = self.module_logs.iter_mut().find(|l| l.id == log_id) {
            log.status = if code == Some(0) {
                ModuleStatus::Succeeded
            } else {
                ModuleStatus::Failed
            };
            log.code = code;
            log.out = out;
            log.err = err;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::persist::TEST_ENV_LOCK;
    use std::time::{Duration, Instant};

    #[test]
    fn link_then_run_action_captures_output() {
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("bohay-modtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("BOHAY_HOME", &home);

        // A module dir: manifest + one echo action.
        let dir = home.join("echo-mod");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("bohay-module.toml"),
            r#"
id = "you.echo"
name = "Echo"
version = "0.1.0"
min_bohay_version = "0.1.0"

[[actions]]
id = "refresh"
title = "Refresh"
command = ["sh", "-c", "echo hello-from-module; echo oops 1>&2"]
"#,
        )
        .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();

        // Link it, then it shows runnable and exposes the action.
        let id = app.module_link_with(&dir, true, None).unwrap();
        assert_eq!(id, "you.echo");
        assert!(app.modules.find(&id).unwrap().is_runnable());

        // Invoke the action; pump the loop until its log resolves.
        let log_id = app.module_invoke_action("refresh", None, "test").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
                app.handle_event(ev);
            }
            let resolved = app
                .module_logs
                .iter()
                .find(|l| l.id == log_id)
                .is_some_and(|l| l.status != ModuleStatus::Running);
            if resolved || Instant::now() > deadline {
                break;
            }
        }

        let log = app.module_logs.iter().find(|l| l.id == log_id).unwrap();
        assert_eq!(log.status, ModuleStatus::Succeeded, "stderr: {}", log.err);
        assert_eq!(log.code, Some(0));
        assert!(
            log.out.contains("hello-from-module"),
            "captured stdout: {:?}",
            log.out
        );
        assert!(log.err.contains("oops"), "captured stderr: {:?}", log.err);

        // Disabling makes it non-runnable; unlink removes it.
        app.module_set_enabled(&id, false).unwrap();
        assert!(!app.modules.find(&id).unwrap().is_runnable());
        assert!(app.module_invoke_action("refresh", None, "test").is_err());
        // Naming the module explicitly gives a clear "disabled" error.
        let err = app
            .module_invoke_action("refresh", Some(&id), "test")
            .unwrap_err();
        assert!(err.contains("disabled"), "got: {err}");
        app.module_unlink(&id).unwrap();
        assert!(app.modules.find(&id).is_none());

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn open_module_pane_tracks_and_cleans_up() {
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("bohay-panetest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("BOHAY_HOME", &home);

        let dir = home.join("board-mod");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("bohay-module.toml"),
            r#"
id = "you.board"
name = "Board"
version = "0.1.0"
min_bohay_version = "0.1.0"

[[panes]]
id = "board"
title = "Board"
command = ["sh", "-c", "sleep 5"]
"#,
        )
        .unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.module_link_with(&dir, true, None).unwrap();

        let before = app.panes.len();
        let pid = app
            .module_open_pane("you.board", "board", Some("split"), "test")
            .unwrap();
        assert_eq!(app.panes.len(), before + 1, "a real pane was spawned");
        assert!(
            app.module_panes.contains_key(&pid),
            "tracked as a module pane"
        );
        assert!(
            app.layout().leaves().contains(&pid),
            "the module pane is in the layout"
        );

        // A missing entrypoint is an error.
        assert!(app
            .module_open_pane("you.board", "nope", None, "test")
            .is_err());

        // Closing the pane untracks it.
        app.close_pane(pid);
        assert!(!app.panes.contains_key(&pid));
        assert!(!app.module_panes.contains_key(&pid), "record auto-removed");

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn event_hook_runs_with_event_env() {
        use std::time::{Duration, Instant};
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("bohay-evtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("BOHAY_HOME", &home);

        let dir = home.join("notify-mod");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("bohay-module.toml"),
            r#"
id = "you.notify"
name = "Notify"
version = "0.1.0"
min_bohay_version = "0.1.0"

[[events]]
on = "pane.agent_status_changed"
command = ["sh", "-c", "echo event=$BOHAY_MODULE_EVENT json=$BOHAY_MODULE_EVENT_JSON"]
"#,
        )
        .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.module_link_with(&dir, true, None).unwrap();

        // Firing the event runs the hook (and publishes to subscribers).
        app.emit_event(
            "pane.agent_status_changed",
            serde_json::json!({"pane": "1", "status": "blocked", "agent": "claude"}),
        );
        let log_id = app
            .module_logs
            .iter()
            .find(|l| l.label == "event:pane.agent_status_changed")
            .map(|l| l.id)
            .expect("a hook command was queued");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
                app.handle_event(ev);
            }
            let resolved = app
                .module_logs
                .iter()
                .find(|l| l.id == log_id)
                .is_some_and(|l| l.status != ModuleStatus::Running);
            if resolved || Instant::now() > deadline {
                break;
            }
        }

        let log = app.module_logs.iter().find(|l| l.id == log_id).unwrap();
        assert_eq!(log.status, ModuleStatus::Succeeded, "stderr: {}", log.err);
        assert!(
            log.out.contains("event=pane.agent_status_changed"),
            "event name injected: {:?}",
            log.out
        );
        assert!(
            log.out.contains("blocked"),
            "event json injected: {:?}",
            log.out
        );

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn module_pane_survives_snapshot_restore() {
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("bohay-restoretest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("BOHAY_HOME", &home);

        let dir = home.join("board-mod");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("bohay-module.toml"),
            r#"
id = "you.board"
name = "Board"
version = "0.1.0"
min_bohay_version = "0.1.0"

[[panes]]
id = "board"
title = "Board"
command = ["sh", "-c", "sleep 5"]
"#,
        )
        .unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.module_link_with(&dir, true, None).unwrap();
        let pid = app
            .module_open_pane("you.board", "board", Some("split"), "test")
            .unwrap();
        assert!(app.module_panes.contains_key(&pid));

        // Snapshot, then restore into a fresh App.
        let snap = crate::persist::snapshot(&app);
        let (tx2, _rx2) = std::sync::mpsc::channel();
        let restored = App::from_snapshot(snap, tx2).expect("restore");

        // The module pane came back as a module pane (not a plain shell).
        let rec = restored
            .module_panes
            .iter()
            .find(|(_, r)| r.module_id == "you.board" && r.entrypoint == "board");
        assert!(rec.is_some(), "module pane was restored as a module pane");
        let (rid, _) = rec.unwrap();
        assert_eq!(
            restored.panes.get(rid).map(|p| p.command.as_str()),
            Some("sh"),
            "it re-ran the module command, not the login shell"
        );

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn settings_modules_tab_lists_and_toggles() {
        use ratatui::backend::TestBackend;
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::Terminal;

        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("bohay-modtab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("BOHAY_HOME", &home);

        for n in ["alpha", "beta"] {
            let dir = home.join(n);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("bohay-module.toml"),
                format!("id = \"you.{n}\"\nname = \"{n}\"\nversion = \"0.1.0\"\nmin_bohay_version = \"0.1.0\"\n"),
            )
            .unwrap();
        }

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.module_link_with(&home.join("alpha"), true, None)
            .unwrap();
        app.module_link_with(&home.join("beta"), true, None)
            .unwrap();

        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.open_settings();
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('5'),
            KeyModifiers::NONE,
        ))); // Modules (tab 5 after Theme/Layout/Notify/Keys)
        term.draw(|f| crate::ui::render(f, &mut app)).unwrap();

        assert_eq!(app.settings_ctl_rects.len(), 2, "one row per module");
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("you.alpha") && text.contains("you.beta"));

        // Clicking a module row toggles its enabled flag (and persists).
        let before = app.modules.find("you.alpha").unwrap().enabled;
        let row = app
            .settings_ctl_rects
            .iter()
            .find(|(i, _)| *i == 0)
            .unwrap()
            .1;
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: row.x + 2,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert_ne!(app.modules.find("you.alpha").unwrap().enabled, before);
        assert_eq!(
            crate::module::registry::load()
                .find("you.alpha")
                .unwrap()
                .enabled,
            !before
        );

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn git_install_builds_and_uninstall_removes_checkout() {
        use std::process::Command;
        let _env = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = std::env::temp_dir().join(format!("bohay-gittest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("BOHAY_HOME", &home);

        // A local "remote" git repo with a manifest + a build step.
        let remote = home.join("remote");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::write(
            remote.join("bohay-module.toml"),
            r#"
id = "you.installed"
name = "Installed"
version = "0.1.0"
min_bohay_version = "0.1.0"

[[build]]
command = ["sh", "-c", "touch built.txt"]

[[actions]]
id = "hello"
title = "Hello"
command = ["echo", "hi"]
"#,
        )
        .unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&remote)
                .output()
                .expect("git available")
        };
        git(&["init", "-q"]);
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);

        // Install from the local repo (file:// supports --depth).
        let url = format!("file://{}", remote.display());
        let installed = crate::module::install::install(&url, None, true).expect("install");
        assert_eq!(installed.id, "you.installed");
        assert!(
            installed.source.contains('@'),
            "pinned source: {}",
            installed.source
        );
        assert!(installed.root.exists());
        assert!(
            crate::module::install::is_removable(&installed.root),
            "landed in the managed dir"
        );
        assert!(
            installed.root.join("built.txt").exists(),
            "the [[build]] step ran"
        );

        // Register + uninstall via the App; the checkout is deleted.
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.module_link_with(&installed.root, true, Some(installed.source.clone()))
            .unwrap();
        assert!(app.modules.find("you.installed").is_some());
        app.module_uninstall("you.installed").unwrap();
        assert!(app.modules.find("you.installed").is_none());
        assert!(!installed.root.exists(), "managed checkout removed");

        std::env::remove_var("BOHAY_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
