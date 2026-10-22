//! Bridge to Claude Code hooks. `atoll hook …` (see hook.rs) connects to a
//! Unix socket owned by this process, sends one request as a JSON line and,
//! for prompts, waits for one reply line. Requests reach the UI through a
//! channel; the UI answers through the request's own reply channel.
//!
//! Fail-safe by design: no socket, no answer, or any error simply means the
//! helper prints nothing and Claude Code shows its normal prompt.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How long a prompt waits for a click before giving up. The hook's own
/// timeout in Claude Code's settings is set higher, so this always wins.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);
/// How often a waiting call checks that its hook is still there.
const HOOK_CHECK: Duration = Duration::from_millis(300);

pub fn socket_path() -> Option<PathBuf> {
    // XDG_RUNTIME_DIR is per-user and mode 0700, so only this user can
    // reach the socket.
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    Some(dir.join("atoll.sock"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Request {
    /// A tool permission prompt.
    Permission {
        tool: String,
        /// What it will do, e.g. the shell command or the file path.
        summary: String,
        project: String,
    },
    /// AskUserQuestion: one or more questions with options.
    Question {
        questions: Vec<Question>,
        project: String,
    },
    /// Claude finished a response.
    Finished { message: String, project: String },
    /// Claude has been waiting for input for a while.
    Waiting { message: String, project: String },
    /// A long terminal command finished (from the shell integration, not
    /// Claude; it shares the socket).
    CommandDone {
        command: String,
        status: i32,
        seconds: u64,
        project: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Question {
    pub question: String,
    #[serde(default)]
    pub header: String,
    pub options: Vec<Choice>,
    #[serde(default, rename = "multiSelect")]
    pub multi_select: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Allow,
    Deny,
    /// question text -> chosen label(s), comma-separated for multi-select.
    Answers(std::collections::HashMap<String, String>),
}

impl Request {
    pub fn wants_reply(&self) -> bool {
        matches!(self, Request::Permission { .. } | Request::Question { .. })
    }
}

/// A request on its way to the UI. Dropping `reply` without sending means
/// "no answer from Atoll".
pub struct Incoming {
    pub request: Request,
    pub reply: async_channel::Sender<Reply>,
}

pub fn spawn(requests: async_channel::Sender<Incoming>) {
    let Some(path) = socket_path() else {
        tracing::warn!("no XDG_RUNTIME_DIR; Claude bridge disabled");
        return;
    };
    // A socket left by a crashed run would make bind fail.
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                "Claude bridge disabled, cannot bind {}: {e}",
                path.display()
            );
            return;
        }
    };
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    tracing::debug!("Claude bridge listening on {}", path.display());

    let spawned = std::thread::Builder::new()
        .name("claude".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let requests = requests.clone();
                // One short-lived thread per hook call; it ends when the call
                // is answered, times out, or the helper goes away.
                let _ = std::thread::Builder::new()
                    .name("claude-call".into())
                    .spawn(move || serve(stream, &requests));
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("cannot start Claude bridge thread: {e}");
    }
}

fn serve(stream: UnixStream, requests: &async_channel::Sender<Incoming>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return;
    }
    let request: Request = match serde_json::from_str(&line) {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("bad hook request: {e}");
            return;
        }
    };
    let wants_reply = request.wants_reply();
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if requests
        .send_blocking(Incoming {
            request,
            reply: reply_tx,
        })
        .is_err()
    {
        return;
    }
    if !wants_reply {
        return;
    }
    // Wait for the answer, but also watch the hook: if it goes away (the
    // user answered in the terminal or VS Code, or Claude Code gave up),
    // close our end so the notch drops the prompt at once.
    let _ = stream.set_nonblocking(true);
    let mut stream = stream;
    loop {
        match reply_rx.try_recv() {
            Ok(reply) => {
                if let Ok(mut json) = serde_json::to_string(&reply) {
                    json.push('\n');
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.write_all(json.as_bytes());
                }
                return;
            }
            // The UI dropped it (timeout, feature off): nothing to send.
            Err(async_channel::TryRecvError::Closed) => return,
            Err(async_channel::TryRecvError::Empty) => {}
        }
        let mut probe = [0u8; 1];
        match stream.read(&mut probe) {
            Ok(0) => return, // hook closed its end
            Err(e) if e.kind() != std::io::ErrorKind::WouldBlock => return,
            _ => {}
        }
        std::thread::sleep(HOOK_CHECK);
    }
}
