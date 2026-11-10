//! Integration tests: run the real `atoll` binary the way Claude Code, the
//! shell and a user would, in a throwaway home so nothing touches the real
//! one.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn atoll() -> Command {
    Command::new(env!("CARGO_BIN_EXE_atoll"))
}

/// A private, empty home and runtime folder for one test.
fn sandbox(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("atoll-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("run")).unwrap();
    dir
}

fn isolated(cmd: &mut Command, home: &PathBuf) {
    cmd.env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME");
}

fn hook(event: &str, input: &str, home: &PathBuf) -> std::process::Output {
    let mut cmd = atoll();
    isolated(&mut cmd, home);
    let mut child = cmd
        .args(["hook", event])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn hook_without_atoll_running_says_nothing() {
    // No socket: Claude Code must get no output and carry on as usual.
    let home = sandbox("nosocket");
    let out = hook(
        "permission",
        r#"{"tool_name":"Bash","tool_input":{"command":"ls"},"cwd":"/tmp"}"#,
        &home,
    );
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn hook_ignores_garbage_input() {
    let home = sandbox("garbage");
    let out = hook("question", "not json at all", &home);
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn shell_snippet_is_valid_bash() {
    let out = atoll().arg("shell-init").output().unwrap();
    assert!(out.status.success());
    let mut bash = Command::new("bash")
        .arg("-n")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    bash.stdin.take().unwrap().write_all(&out.stdout).unwrap();
    assert!(bash.wait().unwrap().success(), "snippet doesn't parse");
}

#[test]
fn a_card_renders_off_screen() {
    // Needs an X display (CI provides a virtual one).
    if std::env::var_os("DISPLAY").is_none() {
        eprintln!("skipped: no DISPLAY");
        return;
    }
    let home = sandbox("render");
    let shot = home.join("todo.png");
    // A render can occasionally come out empty; a few tries.
    for _ in 0..4 {
        let mut cmd = atoll();
        isolated(&mut cmd, &home);
        let _ = cmd
            .env("ATOLL_SHOT", &shot)
            .env("ATOLL_SHOW", "todo")
            .env("ATOLL_SHOT_DELAY_MS", "1500")
            .env("GDK_BACKEND", "x11")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .and_then(|mut c| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                loop {
                    if c.try_wait()?.is_some() || std::time::Instant::now() > deadline {
                        let _ = c.kill();
                        return c.wait();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            });
        if shot.exists() {
            break;
        }
    }
    let size = std::fs::metadata(&shot).map(|m| m.len()).unwrap_or(0);
    assert!(size > 2_000, "no card rendered ({size} bytes)");
}
