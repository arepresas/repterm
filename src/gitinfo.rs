use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitStatus {
    pub branch: String,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoStatus {
    pub git: Option<GitStatus>,
    pub pr: Option<PullRequest>,
}

pub fn expand_home(path: &str, home: Option<&str>) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    if path == "~" {
        return home.map(str::to_owned);
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.map(|home| format!("{home}/{rest}"));
    }
    path.starts_with('/').then(|| path.to_owned())
}

pub fn parse_git_status(output: &str) -> Option<GitStatus> {
    let mut branch = None;
    let mut ahead = 0;
    let mut behind = 0;
    let mut dirty = false;
    for line in output.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            branch = Some(head.trim().to_owned());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            (ahead, behind) = parse_ahead_behind(ab);
        } else if !line.starts_with('#') && !line.trim().is_empty() {
            dirty = true;
        }
    }
    branch
        .filter(|name| !name.is_empty())
        .map(|branch| GitStatus {
            branch,
            dirty,
            ahead,
            behind,
        })
}

pub fn format_git_label(status: &GitStatus) -> String {
    let mut label = status.branch.clone();
    if status.dirty {
        label.push_str(" *");
    }
    if status.ahead > 0 {
        label.push_str(&format!(" +{}", status.ahead));
    }
    if status.behind > 0 {
        label.push_str(&format!(" -{}", status.behind));
    }
    label
}

pub fn parse_pr_json(json: &str) -> Option<PullRequest> {
    let number = json_u64(json, "number")?;
    let title = json_string(json, "title")?;
    let url = json_string(json, "url")?;
    let state = json_string(json, "state").unwrap_or_else(|| "OPEN".to_owned());
    is_http_url(&url).then_some(PullRequest {
        number,
        title,
        url,
        state,
    })
}

pub fn is_http_url(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url.starts_with("https://") || url.starts_with("http://")
}

pub fn probe(path: &str) -> RepoStatus {
    let git = git_status(path);
    let pr = git.as_ref().and_then(|_| pr_view(path));
    RepoStatus { git, pr }
}

pub fn open_url(url: &str) -> bool {
    if !is_http_url(url) {
        return false;
    }
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn git_status(path: &str) -> Option<GitStatus> {
    let mut command = Command::new("git");
    command.args(["-C", path, "status", "--porcelain=v2", "--branch"]);
    parse_git_status(&command_stdout(&mut command)?)
}

fn pr_view(path: &str) -> Option<PullRequest> {
    let mut command = Command::new("gh");
    command
        .args(["pr", "view", "--json", "number,title,url,state"])
        .current_dir(path);
    parse_pr_json(&command_stdout(&mut command)?)
}

fn command_stdout(command: &mut Command) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = read_stdout(child.stdout.take());
                return status.success().then_some(stdout);
            }
            Ok(None) if started.elapsed() > COMMAND_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
}

fn read_stdout(pipe: Option<impl Read>) -> String {
    let mut stdout = String::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_string(&mut stdout);
    }
    stdout
}

fn parse_ahead_behind(text: &str) -> (u32, u32) {
    let mut ahead = 0;
    let mut behind = 0;
    for part in text.split_whitespace() {
        if let Some(value) = part.strip_prefix('+') {
            ahead = value.parse().unwrap_or(0);
        } else if let Some(value) = part.strip_prefix('-') {
            behind = value.parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

fn json_string(json: &str, key: &str) -> Option<String> {
    let rest = field_value(json, key)?;
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            _ => out.push(ch),
        }
    }
    None
}

fn json_u64(json: &str, key: &str) -> Option<u64> {
    let rest = field_value(json, key)?;
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn field_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let index = json.find(&needle)?;
    let rest = json[index + needle.len()..].trim_start();
    Some(rest.strip_prefix(':')?.trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_and_keeps_absolute_path() {
        assert_eq!(
            expand_home("~/Dev/repterm", Some("/home/overlag")),
            Some("/home/overlag/Dev/repterm".to_owned())
        );
        assert_eq!(
            expand_home("/tmp", Some("/home/overlag")),
            Some("/tmp".to_owned())
        );
    }

    #[test]
    fn rejects_relative_or_empty_path() {
        assert_eq!(expand_home("vim", Some("/home/overlag")), None);
        assert_eq!(expand_home("  ", Some("/home/overlag")), None);
        assert_eq!(expand_home("~/x", None), None);
    }

    #[test]
    fn parses_branch_dirty_and_divergence() {
        let status = parse_git_status(
            "# branch.oid abc\n# branch.head main\n# branch.ab +1 -2\n1 .M N... file\n",
        )
        .expect("status");
        assert_eq!(status.branch, "main");
        assert!(status.dirty);
        assert_eq!(status.ahead, 1);
        assert_eq!(status.behind, 2);
        assert_eq!(format_git_label(&status), "main * +1 -2");
    }

    #[test]
    fn clean_branch_has_no_markers() {
        let status = parse_git_status("# branch.head dev\n# branch.ab +0 -0\n").expect("status");
        assert!(!status.dirty);
        assert_eq!(format_git_label(&status), "dev");
    }

    #[test]
    fn missing_branch_is_not_a_repo() {
        assert_eq!(parse_git_status("fatal: not a git repository\n"), None);
    }

    #[test]
    fn parses_pull_request_json() {
        let pr = parse_pr_json(
            r#"{"number":12,"state":"OPEN","title":"fix tabs","url":"https://github.com/o/r/pull/12"}"#,
        )
        .expect("pr");
        assert_eq!(pr.number, 12);
        assert_eq!(pr.state, "OPEN");
        assert_eq!(pr.url, "https://github.com/o/r/pull/12");
    }

    #[test]
    fn rejects_non_http_pull_request_url() {
        assert_eq!(
            parse_pr_json(r#"{"number":1,"title":"x","url":"file:///tmp","state":"OPEN"}"#),
            None
        );
    }
}
