pub fn resolve_shell(shell: Option<&str>) -> String {
    #[cfg(windows)]
    {
        let _ = shell;
        "cmd.exe".to_owned()
    }
    #[cfg(not(windows))]
    {
        match shell {
            Some(value) if !value.trim().is_empty() => value.to_owned(),
            _ => "/bin/sh".to_owned(),
        }
    }
}

fn shell_name(shell: &str) -> &str {
    shell.rsplit('/').next().unwrap_or(shell)
}

/// zsh can enable this at spawn time, so no setup line is echoed into the terminal.
pub fn shell_args(shell: &str) -> Vec<String> {
    match shell_name(shell) {
        "zsh" => vec!["-o".to_owned(), "HIST_IGNORE_SPACE".to_owned()],
        _ => Vec::new(),
    }
}

/// bash has no spawn-time equivalent, so it needs one setup line at startup.
pub fn history_setup_command(shell: &str) -> Option<String> {
    match shell_name(shell) {
        "bash" => Some("HISTCONTROL=\"${HISTCONTROL:+$HISTCONTROL:}ignorespace\"\n".to_owned()),
        _ => None,
    }
}

/// Agent prompts are shown with `!` in the terminal, so their history entries
/// keep that prefix too, even when sent from an ongoing agent conversation.
pub fn history_entry_text(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.starts_with('!') {
        trimmed.to_owned()
    } else {
        format!("!{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn uses_shell_when_present() {
        assert_eq!(resolve_shell(Some("/bin/bash")), "/bin/bash");
    }

    #[cfg(not(windows))]
    #[test]
    fn empty_shell_falls_back() {
        assert_eq!(resolve_shell(Some("")), "/bin/sh");
    }

    #[cfg(not(windows))]
    #[test]
    fn whitespace_shell_falls_back() {
        assert_eq!(resolve_shell(Some("   ")), "/bin/sh");
    }

    #[cfg(not(windows))]
    #[test]
    fn missing_shell_falls_back() {
        assert_eq!(resolve_shell(None), "/bin/sh");
    }

    #[cfg(windows)]
    #[test]
    fn ignores_shell_on_windows() {
        assert_eq!(resolve_shell(Some("/usr/bin/bash")), "cmd.exe");
    }

    #[test]
    fn zsh_enables_ignore_space_at_spawn() {
        assert_eq!(
            shell_args("/usr/bin/zsh"),
            vec!["-o".to_owned(), "HIST_IGNORE_SPACE".to_owned()]
        );
        assert!(shell_args("/bin/bash").is_empty());
    }

    #[test]
    fn only_bash_needs_a_setup_line() {
        assert_eq!(
            history_setup_command("/bin/bash").as_deref(),
            Some("HISTCONTROL=\"${HISTCONTROL:+$HISTCONTROL:}ignorespace\"\n")
        );
        assert_eq!(history_setup_command("/usr/bin/zsh"), None);
    }

    #[test]
    fn history_entries_always_start_with_bang() {
        assert_eq!(
            history_entry_text("tell me what's there"),
            "!tell me what's there"
        );
        assert_eq!(
            history_entry_text(" !tell me what's there "),
            "!tell me what's there"
        );
        assert_eq!(history_entry_text("!!thing"), "!!thing");
    }
}
