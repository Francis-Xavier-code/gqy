use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn miyu_command(binary: &Path, home: &Path, runtime: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env("MIYU_HOME", home)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("MIYU_LANG", "en");
    command
}

struct DaemonGuard {
    binary: PathBuf,
    home: PathBuf,
    runtime: PathBuf,
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = miyu_command(&self.binary, &self.home, &self.runtime)
            .args(["daemon", "stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run(binary: &Path, home: &Path, runtime: &Path, args: &[&str]) -> Output {
    miyu_command(binary, home, runtime)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn reload_fails_without_starting_a_daemon() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_miyu"));

    assert!(run(&binary, &home, &runtime, &["init"]).status.success());
    let reload = run(&binary, &home, &runtime, &["reload"]);
    assert!(!reload.status.success());
    assert!(String::from_utf8_lossy(&reload.stderr).contains("Miyu daemon is not running"));

    let status = run(&binary, &home, &runtime, &["daemon", "status"]);
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("Miyu daemon: stopped"));
}

#[test]
fn reload_applies_config_without_restarting_daemon() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let runtime = temp.path().join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_miyu"));

    let init = run(&binary, &home, &runtime, &["init"]);
    assert!(init.status.success(), "miyu init failed: {init:?}");

    let child = miyu_command(&binary, &home, &runtime)
        .args(["__daemon", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut daemon = DaemonGuard {
        binary: binary.clone(),
        home: home.clone(),
        runtime: runtime.clone(),
        child,
    };

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "daemon exited before becoming ready"
        );
        let status = run(&binary, &home, &runtime, &["daemon", "status"]);
        if status.status.success()
            && String::from_utf8_lossy(&status.stdout).contains("Miyu daemon: running")
        {
            break;
        }
        assert!(Instant::now() < deadline, "daemon did not become ready");
        thread::sleep(Duration::from_millis(50));
    }

    let config_path = home.join("config/config.jsonc");
    let mut config: Value = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let previous = config["display"]["show_token_usage"]
        .as_bool()
        .unwrap_or(false);
    config["display"]["show_token_usage"] = Value::Bool(!previous);
    std::fs::write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&config).unwrap()),
    )
    .unwrap();

    let reload = run(&binary, &home, &runtime, &["reload"]);
    assert!(reload.status.success(), "miyu reload failed: {reload:?}");
    assert!(String::from_utf8_lossy(&reload.stdout).contains("configuration reloaded"));
    assert_eq!(daemon.child.id(), pid);
    assert!(daemon.child.try_wait().unwrap().is_none());
}
