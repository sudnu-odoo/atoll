//! `atoll hook <event>`: the command Claude Code runs as a hook. Reads the
//! hook's JSON from stdin, forwards it to the running Atoll, and prints
//! Claude Code's decision JSON if (and only if) the user answered in the
//! notch. Never starts GTK.
//!
//! Every failure path prints nothing and exits 0, which Claude Code treats
//! as "no opinion": its normal prompt appears as if this hook didn't exist.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde_json::{json, Value};

use crate::services::claude::{self, Question, Reply, Request};

pub fn run(event: &str) {
    if let Some(output) = handle(event) {
        println!("{output}");
    }
}

fn handle(event: &str) -> Option<Value> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).ok()?;
    let hook: Value = serde_json::from_str(&input).ok()?;
    let project = hook["cwd"]
        .as_str()
        .and_then(|c| Path::new(c).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let request = match event {
        "permission" => Request::Permission {
            tool: hook["tool_name"].as_str()?.to_owned(),
            summary: summarize(&hook["tool_input"]),
            project,
        },
        "question" => Request::Question {
            questions: serde_json::from_value::<Vec<Question>>(
                hook["tool_input"]["questions"].clone(),
            )
            .ok()
            .filter(|q| !q.is_empty())?,
            project,
        },
        "stop" => {
            // A Stop hook that is itself continuing a stop: not a new finish.
            if hook["stop_hook_active"].as_bool() == Some(true) {
                return None;
            }
            Request::Finished {
                message: first_line(hook["last_assistant_message"].as_str().unwrap_or("")),
                project,
            }
        }
        "notification" => {
            // Permission prompts already come through the permission hook.
            if hook["notification_type"].as_str() != Some("idle_prompt") {
                return None;
            }
            Request::Waiting {
                message: hook["message"].as_str().unwrap_or("").to_owned(),
                project,
            }
        }
        _ => return None,
    };

    let reply = exchange(&request)?;
    decision(&request, &hook, reply)
}

/// Sends the request; for prompts, waits for the answer.
fn exchange(request: &Request) -> Option<Reply> {
    let mut stream = UnixStream::connect(claude::socket_path()?).ok()?;
    let mut line = serde_json::to_string(request).ok()?;
    line.push('\n');
    stream.write_all(line.as_bytes()).ok()?;
    if !request.wants_reply() {
        return None;
    }
    // A little past Atoll's own timeout, so Atoll gives up first.
    let _ = stream.set_read_timeout(Some(
        claude::ANSWER_TIMEOUT + std::time::Duration::from_secs(3),
    ));
    let mut answer = String::new();
    BufReader::new(stream).read_line(&mut answer).ok()?;
    serde_json::from_str(&answer).ok()
}

/// Claude Code's hook output for the user's answer.
fn decision(request: &Request, hook: &Value, reply: Reply) -> Option<Value> {
    match (request, reply) {
        (Request::Permission { .. }, Reply::Allow) => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "allow" }
            }
        })),
        (Request::Permission { .. }, Reply::Deny) => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "deny", "message": "Denied from the Atoll notch" }
            }
        })),
        (Request::Question { .. }, Reply::Answers(answers)) => {
            // updatedInput replaces the whole input: keep the original
            // questions and add the answers.
            let mut input = hook["tool_input"].clone();
            input["answers"] = json!(answers);
            Some(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                    "permissionDecisionReason": "Answered in the Atoll notch",
                    "updatedInput": input
                }
            }))
        }
        _ => None,
    }
}

/// One line describing what a tool call will do.
fn summarize(input: &Value) -> String {
    for key in ["command", "file_path", "path", "url", "pattern", "query"] {
        if let Some(s) = input[key].as_str() {
            return s.to_owned();
        }
    }
    let compact = input.to_string();
    if compact.chars().count() > 200 {
        compact.chars().take(200).chain("…".chars()).collect()
    } else {
        compact
    }
}

fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .trim_start_matches('#')
        .trim();
    if line.chars().count() > 120 {
        line.chars().take(120).chain("…".chars()).collect()
    } else {
        line.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission() -> Request {
        Request::Permission {
            tool: "Bash".into(),
            summary: "ls".into(),
            project: "atoll".into(),
        }
    }

    #[test]
    fn allow_and_deny_match_claude_code_schema() {
        let out = decision(&permission(), &json!({}), Reply::Allow).unwrap();
        assert_eq!(
            out["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let out = decision(&permission(), &json!({}), Reply::Deny).unwrap();
        assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "deny");
    }

    #[test]
    fn answers_keep_original_questions() {
        let hook = json!({"tool_input": {"questions": [{"question": "Q?", "options": []}]}});
        let request = Request::Question {
            questions: vec![],
            project: String::new(),
        };
        let answers = [("Q?".to_owned(), "A".to_owned())].into_iter().collect();
        let out = decision(&request, &hook, Reply::Answers(answers)).unwrap();
        let o = &out["hookSpecificOutput"];
        assert_eq!(o["hookEventName"], "PreToolUse");
        assert_eq!(o["permissionDecision"], "allow");
        assert_eq!(o["updatedInput"]["questions"][0]["question"], "Q?");
        assert_eq!(o["updatedInput"]["answers"]["Q?"], "A");
    }

    #[test]
    fn mismatched_reply_gives_no_opinion() {
        assert!(decision(
            &permission(),
            &json!({}),
            Reply::Answers(Default::default())
        )
        .is_none());
    }
}
