//! `atoll shell-init`: prints the bash snippet that reports finished
//! commands to Atoll. Sourced at the end of ~/.bashrc, after the prompt
//! tools (Starship, zoxide), so it can run first and pass the exit status on.
//!
//! It uses no DEBUG trap (Starship owns that); the command's start time comes
//! from bash history with HISTTIMEFORMAT=%s. Commands bash doesn't record
//! (leading space, duplicates) simply aren't reported.

pub fn print_init() {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "atoll".into());
    print!("{}", snippet(&exe));
}

/// Keeps ~/.config/atoll/shell.bash (Atoll's own file, if the user created
/// it) pointing at the installed binary, e.g. after moving from install.sh
/// to the package. Never creates it: that's the user's choice.
pub fn refresh_saved() {
    if cfg!(test) {
        return;
    }
    let (Some(path), Some(exe)) = (
        crate::store::config_file("shell.bash"),
        crate::autostart::installed(),
    ) else {
        return;
    };
    let Ok(old) = std::fs::read_to_string(&path) else {
        return;
    };
    let new = snippet(&exe.to_string_lossy());
    if old != new {
        match std::fs::write(&path, new) {
            Ok(()) => tracing::info!("updated {}", path.display()),
            Err(e) => tracing::warn!("cannot update {}: {e}", path.display()),
        }
    }
}

fn snippet(exe: &str) -> String {
    let exe = exe.replace('\'', r"'\''");
    format!(
        r#"# Atoll: peek when a long command finishes (see `atoll shell-init`).
__atoll_bin='{exe}'
__atoll_last=$(HISTTIMEFORMAT='%s ' builtin history 1 2>/dev/null | awk '{{print $1}}')
__atoll_precmd() {{
  local status=$? num start cmd
  read -r num start cmd <<< "$(HISTTIMEFORMAT='%s ' builtin history 1 2>/dev/null)"
  if [[ -n $num && $num != "$__atoll_last" && $start =~ ^[0-9]+$ ]]; then
    __atoll_last=$num
    ( "$__atoll_bin" hook command "$status" "$start" "$cmd" "$PWD" >/dev/null 2>&1 & )
  fi
  return $status
}}
if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
  PROMPT_COMMAND=(__atoll_precmd "${{PROMPT_COMMAND[@]}}")
else
  PROMPT_COMMAND="__atoll_precmd${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}"
fi
"#
    )
}
