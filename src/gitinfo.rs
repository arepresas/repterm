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
    pub changes: Vec<ChangedFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub is_remote: bool,
    pub is_current: bool,
    pub last_used: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub status: char,
    pub staged: bool,
    pub added: u32,
    pub removed: u32,
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
    let changes = git.as_ref().map_or_else(Vec::new, |_| git_changes(path));
    let pr = git.as_ref().and_then(|_| pr_view(path));
    RepoStatus { git, pr, changes }
}

pub fn parse_branch_list(output: &str) -> Vec<BranchInfo> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(5, '\t');
            let full_ref = fields.next()?.trim();
            let name = fields.next()?.trim();
            let _sha = fields.next()?;
            let last_used = fields.next()?.trim().parse::<i64>().ok()?;
            let _upstream = fields.next()?;
            if name.is_empty() || name.ends_with("/HEAD") {
                return None;
            }
            Some(BranchInfo {
                name: name.to_owned(),
                is_remote: full_ref.starts_with("refs/remotes/"),
                is_current: false,
                last_used,
            })
        })
        .collect()
}

pub fn branches(path: &str) -> Vec<BranchInfo> {
    let mut refs_command = Command::new("git");
    refs_command.args([
        "-C",
        path,
        "--no-pager",
        "for-each-ref",
        "--format=%(refname)%09%(refname:short)%09%(objectname:short)%09%(committerdate:unix)%09%(upstream:short)",
        "refs/heads",
        "refs/remotes",
    ]);
    let Some(output) = command_stdout(&mut refs_command) else {
        return Vec::new();
    };

    let mut items = parse_branch_list(&output);

    let mut current_command = Command::new("git");
    current_command.args(["-C", path, "--no-pager", "branch", "--show-current"]);
    let current = command_stdout(&mut current_command)
        .map(|text| text.trim().to_owned())
        .filter(|name| !name.is_empty());

    if let Some(current) = current {
        for item in &mut items {
            item.is_current = item.name == current;
        }
    }

    items.sort_by(|a, b| {
        b.last_used
            .cmp(&a.last_used)
            .then_with(|| a.name.cmp(&b.name))
    });
    items
}

pub fn parse_change_list(
    status_z: &str,
    staged_numstat_z: &str,
    unstaged_numstat_z: &str,
) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let entries: Vec<&str> = status_z.split('\0').collect();
    let mut index = 0;
    while index < entries.len() {
        let entry = entries[index];
        index += 1;
        if entry.is_empty() || entry.len() < 3 {
            continue;
        }

        let bytes = entry.as_bytes();
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        let path = &entry[3..];
        if path.is_empty() {
            continue;
        }

        let is_rename_or_copy = matches!(x, 'R' | 'C') || matches!(y, 'R' | 'C');
        if is_rename_or_copy && index < entries.len() {
            index += 1;
        }

        let index_changed = x != ' ' && x != '?';
        let worktree_changed = y != ' ' && y != '?';
        if index_changed {
            files.push(ChangedFile {
                path: path.to_owned(),
                status: x,
                staged: true,
                added: 0,
                removed: 0,
            });
        }
        if worktree_changed {
            files.push(ChangedFile {
                path: path.to_owned(),
                status: y,
                staged: false,
                added: 0,
                removed: 0,
            });
        }
        if !index_changed && !worktree_changed {
            files.push(ChangedFile {
                path: path.to_owned(),
                status: '?',
                staged: false,
                added: 0,
                removed: 0,
            });
        }
    }

    apply_numstat(&mut files, staged_numstat_z, true);
    apply_numstat(&mut files, unstaged_numstat_z, false);
    files.sort_by(|a, b| b.staged.cmp(&a.staged).then_with(|| a.path.cmp(&b.path)));
    files
}

pub fn change_totals(files: &[ChangedFile]) -> (usize, u32, u32) {
    let mut paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    paths.sort_unstable();
    paths.dedup();
    files
        .iter()
        .fold((paths.len(), 0, 0), |(count, add, rm), file| {
            (count, add + file.added, rm + file.removed)
        })
}

pub fn diff_for(path: &str, file: &ChangedFile) -> String {
    if file.status == '?' {
        return String::new();
    }
    let mut command = Command::new("git");
    command.args([
        "-C",
        path,
        "--no-pager",
        "diff",
        "--no-ext-diff",
        "--no-color",
    ]);
    if file.staged {
        command.arg("--cached");
    }
    command.arg("--").arg(&file.path);
    command_stdout(&mut command).unwrap_or_default()
}

pub fn checkout_command(branch: &BranchInfo) -> String {
    if branch.is_current {
        return String::new();
    }
    let target = if branch.is_remote {
        branch
            .name
            .split_once('/')
            .map(|(_, rest)| rest)
            .unwrap_or(&branch.name)
    } else {
        &branch.name
    };
    format!("git checkout '{}'\n", escape_single_quotes(target))
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

fn git_changes(path: &str) -> Vec<ChangedFile> {
    let mut status_command = Command::new("git");
    status_command.args([
        "-C",
        path,
        "--no-pager",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
    ]);
    let Some(status_z) = command_stdout(&mut status_command) else {
        return Vec::new();
    };

    let mut staged_numstat_command = Command::new("git");
    staged_numstat_command.args([
        "-C",
        path,
        "--no-pager",
        "diff",
        "--cached",
        "--numstat",
        "-z",
        "--no-ext-diff",
        "--no-color",
    ]);
    let staged_numstat_z = command_stdout(&mut staged_numstat_command).unwrap_or_default();

    let mut unstaged_numstat_command = Command::new("git");
    unstaged_numstat_command.args([
        "-C",
        path,
        "--no-pager",
        "diff",
        "--numstat",
        "-z",
        "--no-ext-diff",
        "--no-color",
    ]);
    let unstaged_numstat_z = command_stdout(&mut unstaged_numstat_command).unwrap_or_default();

    parse_change_list(&status_z, &staged_numstat_z, &unstaged_numstat_z)
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

fn apply_numstat(files: &mut [ChangedFile], numstat_z: &str, staged: bool) {
    let records: Vec<&str> = numstat_z
        .split('\0')
        .filter(|record| !record.is_empty())
        .collect();
    let mut index = 0;
    while index < records.len() {
        let mut fields = records[index].splitn(3, '\t');
        index += 1;
        let (Some(added_raw), Some(removed_raw), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let added = if added_raw == "-" {
            0
        } else {
            added_raw.parse::<u32>().unwrap_or(0)
        };
        let removed = if removed_raw == "-" {
            0
        } else {
            removed_raw.parse::<u32>().unwrap_or(0)
        };

        // Renames have an empty path followed by the old and new paths as
        // separate NUL-delimited records: "5\t2\t\0old\0new\0".
        let paths: Vec<&str> = if path.is_empty() {
            if index + 1 >= records.len() {
                break;
            }
            let paths = vec![records[index], records[index + 1]];
            index += 2;
            paths
        } else {
            vec![path]
        };

        for path in paths {
            if let Some(file) = files
                .iter_mut()
                .find(|file| file.path == path && file.staged == staged)
            {
                file.added = added;
                file.removed = removed;
            }
        }
    }
}

fn escape_single_quotes(value: &str) -> String {
    value.replace('\'', "'\\''")
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

    #[test]
    fn parses_branch_list_and_skips_head_alias() {
        let output = concat!(
            "refs/heads/main\tmain\tabc123\t1700000000\torigin/main\n",
            "refs/heads/feature/foo\tfeature/foo\tbeef11\t1700000003\torigin/feature/foo\n",
            "refs/remotes/origin/main\torigin/main\tdef456\t1700000001\t\n",
            "refs/remotes/origin/HEAD\torigin/HEAD\tfeed00\t1700000002\t\n",
            "bad-line\n"
        );
        let items = parse_branch_list(output);
        assert_eq!(
            items,
            vec![
                BranchInfo {
                    name: "main".to_owned(),
                    is_remote: false,
                    is_current: false,
                    last_used: 1_700_000_000,
                },
                BranchInfo {
                    name: "feature/foo".to_owned(),
                    is_remote: false,
                    is_current: false,
                    last_used: 1_700_000_003,
                },
                BranchInfo {
                    name: "origin/main".to_owned(),
                    is_remote: true,
                    is_current: false,
                    last_used: 1_700_000_001,
                },
            ]
        );
    }

    #[test]
    fn parses_change_list_with_rename_and_untracked() {
        let status_z = "R  src/new.rs\0src/old.rs\0 M src/lib.rs\0?? notes/todo.md\0";
        let staged_numstat_z = "5\t1\tsrc/new.rs\0";
        let unstaged_numstat_z = "3\t2\tsrc/lib.rs\0";
        let items = parse_change_list(status_z, staged_numstat_z, unstaged_numstat_z);
        assert_eq!(
            items,
            vec![
                ChangedFile {
                    path: "src/new.rs".to_owned(),
                    status: 'R',
                    staged: true,
                    added: 5,
                    removed: 1,
                },
                ChangedFile {
                    path: "notes/todo.md".to_owned(),
                    status: '?',
                    staged: false,
                    added: 0,
                    removed: 0,
                },
                ChangedFile {
                    path: "src/lib.rs".to_owned(),
                    status: 'M',
                    staged: false,
                    added: 3,
                    removed: 2,
                },
            ]
        );
    }

    #[test]
    fn computes_change_totals() {
        let files = vec![
            ChangedFile {
                path: "a".to_owned(),
                status: 'M',
                staged: true,
                added: 2,
                removed: 1,
            },
            ChangedFile {
                path: "b".to_owned(),
                status: '?',
                staged: false,
                added: 0,
                removed: 0,
            },
        ];
        assert_eq!(change_totals(&files), (2, 2, 1));
    }

    #[test]
    fn splits_partially_staged_file_into_both_sides() {
        let items = parse_change_list("MM f.txt\0", "1\t1\tf.txt\0", "2\t3\tf.txt\0");
        assert_eq!(
            items,
            vec![
                ChangedFile {
                    path: "f.txt".to_owned(),
                    status: 'M',
                    staged: true,
                    added: 1,
                    removed: 1,
                },
                ChangedFile {
                    path: "f.txt".to_owned(),
                    status: 'M',
                    staged: false,
                    added: 2,
                    removed: 3,
                },
            ]
        );
        assert_eq!(change_totals(&items), (1, 3, 4));
    }

    #[test]
    fn applies_rename_numstat_to_old_and_new_paths() {
        let items = parse_change_list("R  new.txt\0old.txt\0", "5\t2\t\0old.txt\0new.txt\0", "");
        assert_eq!(
            items,
            vec![ChangedFile {
                path: "new.txt".to_owned(),
                status: 'R',
                staged: true,
                added: 5,
                removed: 2,
            }]
        );
    }

    #[test]
    fn builds_checkout_command_with_remote_and_quote_escaping() {
        let remote = BranchInfo {
            name: "origin/feature/it\'s-good".to_owned(),
            is_remote: true,
            is_current: false,
            last_used: 0,
        };
        assert_eq!(
            checkout_command(&remote),
            "git checkout 'feature/it'\\''s-good'\n"
        );

        let current = BranchInfo {
            name: "main".to_owned(),
            is_remote: false,
            is_current: true,
            last_used: 0,
        };
        assert_eq!(checkout_command(&current), "");
    }
}
