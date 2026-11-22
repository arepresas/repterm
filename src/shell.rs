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
}
