use crate::gitinfo::expand_home;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

pub const MAX_READ_BYTES: usize = 65_536;
pub const MAX_WRITE_BYTES: usize = 262_144;
pub const MAX_LIST_ENTRIES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolName {
    ListDir,
    ReadFile,
    WriteFile,
    EditFile,
}

impl ToolName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ListDir => "list_dir",
            Self::ReadFile => "read_file",
            Self::WriteFile => "write_file",
            Self::EditFile => "edit_file",
        }
    }

    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "list_dir" => Some(Self::ListDir),
            "read_file" => Some(Self::ReadFile),
            "write_file" => Some(Self::WriteFile),
            "edit_file" => Some(Self::EditFile),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutcome {
    pub call_id: String,
    pub content: String,
    pub changed_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PermissionKey {
    pub tool: ToolName,
    pub path: PathBuf,
}

fn parameters(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

pub fn tool_definitions() -> Vec<Value> {
    let reason = json!({"type": "string", "description": "Brief reason for the operation"});
    let path = json!({"type": "string", "description": "Path to the file or directory"});
    let definition = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": parameters(properties, required),
            },
        })
    };
    vec![
        definition(
            "list_dir",
            "List one level of a directory inside the project",
            json!({"path": path, "reason": reason}),
            &["path", "reason"],
        ),
        definition(
            "read_file",
            "Read a text file inside the project",
            json!({"path": path, "reason": reason}),
            &["path", "reason"],
        ),
        definition(
            "write_file",
            "Create or overwrite a text file inside the project",
            json!({"path": path, "content": {"type": "string"}, "reason": reason}),
            &["path", "content", "reason"],
        ),
        definition(
            "edit_file",
            "Replace a single occurrence of text in a file",
            json!({
                "path": path,
                "old": {"type": "string"},
                "new": {"type": "string"},
                "reason": reason,
            }),
            &["path", "old", "new", "reason"],
        ),
    ]
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    result.push("..");
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
}

pub fn resolve_path(raw: &str, cwd: &Path, home: Option<&str>) -> Result<PathBuf, String> {
    let path = match expand_home(raw, home) {
        Some(expanded) => PathBuf::from(expanded),
        None => cwd.join(raw),
    };
    let path = normalize(&path);
    let mut missing = Vec::new();
    let mut ancestor = path.as_path();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(ancestor) {
            let mut resolved = canonical;
            for name in missing.iter().rev() {
                resolved.push(name);
            }
            return Ok(resolved);
        }
        match ancestor.file_name() {
            Some(name) => {
                missing.push(name.to_os_string());
                ancestor = ancestor.parent().unwrap_or(Path::new("/"));
            }
            None => return Err("Could not resolve path".into()),
        }
    }
}

pub fn is_within(path: &Path, cwd: &Path) -> bool {
    path.starts_with(cwd)
}

fn relative_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn string_field(args: &Value, name: &str) -> Result<String, String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("Falta el argumento '{name}'"))
}

fn read_text(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("File is not valid UTF-8: {e}"))
}

fn read_file(path: &Path) -> Result<String, String> {
    let text = read_text(path)?;
    if text.len() <= MAX_READ_BYTES {
        return Ok(text);
    }
    let mut end = MAX_READ_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(format!(
        "{}\n[truncado a {} bytes de {}]",
        &text[..end],
        MAX_READ_BYTES,
        text.len()
    ))
}

fn write_file(path: &Path, content: &str) -> Result<(), String> {
    if content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Content exceeds the limit of {} bytes",
            MAX_WRITE_BYTES
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Could not create directory: {e}"))?;
    }
    std::fs::write(path, content).map_err(|e| format!("Could not write: {e}"))
}

fn edit_file(path: &Path, old: &str, new: &str) -> Result<(), String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("Could not read: {e}"))?;
    if metadata.len() as usize > MAX_WRITE_BYTES {
        return Err(format!(
            "File exceeds the limit of {} bytes",
            MAX_WRITE_BYTES
        ));
    }
    let text = read_text(path)?;
    let updated = text.replacen(old, new, 1);
    if updated.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Result exceeds the limit of {} bytes",
            MAX_WRITE_BYTES
        ));
    }
    if text.matches(old).count() != 1 {
        return Err("El texto a reemplazar debe aparecer exactamente una vez".into());
    }
    std::fs::write(path, updated).map_err(|e| format!("Could not write: {e}"))
}

fn list_dir(path: &Path) -> Result<String, String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path).map_err(|e| format!("Could not list: {e}"))? {
        let entry = entry.map_err(|e| format!("Could not list: {e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = entry
            .metadata()
            .map_err(|e| format!("Could not list: {e}"))?;
        if metadata.is_dir() {
            entries.push(format!("{name}/"));
        } else {
            entries.push(format!("{name} ({})", metadata.len()));
        }
    }
    entries.sort();
    let total = entries.len();
    entries.truncate(MAX_LIST_ENTRIES);
    let mut content = entries.join("\n");
    if total > MAX_LIST_ENTRIES {
        content.push_str(&format!(
            "\n[truncated to {MAX_LIST_ENTRIES} of {total} entries]"
        ));
    }
    Ok(content)
}

pub fn execute(call: &ToolCall, cwd: &Path, home: Option<&str>) -> Result<ToolOutcome, String> {
    let args: Value = serde_json::from_str(&call.arguments)
        .map_err(|e| format!("Invalid JSON arguments: {e}"))?;
    string_field(&args, "reason")?;
    ToolName::from_str(&call.name).ok_or_else(|| format!("Unknown tool: {}", call.name))?;
    let raw = string_field(&args, "path")?;
    let path = resolve_path(&raw, cwd, home)?;
    let (content, changed_path) = match call.name.as_str() {
        "list_dir" => (list_dir(&path)?, None),
        "read_file" => (read_file(&path)?, None),
        "write_file" => {
            let content = string_field(&args, "content")?;
            write_file(&path, &content)?;
            (
                format!("File written: {raw}"),
                Some(relative_path(&path, cwd)),
            )
        }
        "edit_file" => {
            let old = string_field(&args, "old")?;
            let new = string_field(&args, "new")?;
            edit_file(&path, &old, &new)?;
            (
                format!("File modified: {raw}"),
                Some(relative_path(&path, cwd)),
            )
        }
        _ => return Err(format!("Unknown tool: {}", call.name)),
    };
    Ok(ToolOutcome {
        call_id: call.id.clone(),
        content,
        changed_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn canonicalized_symlink_outside_cwd_is_not_within() {
        let root = std::env::temp_dir().join(format!("repterm-tools-{}", std::process::id()));
        let inside = root.join("repo");
        let outside = root.join("outside");
        std::fs::create_dir_all(inside.join("link_target").parent().unwrap()).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let _ = std::fs::remove_dir_all(&inside);
        std::fs::create_dir_all(&inside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, inside.join("escape")).unwrap();
        let target = resolve_path("escape/file.txt", &inside, None).unwrap();
        assert!(!is_within(&target, &inside));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn dotdot_and_absolute_paths_resolve_outside_cwd() {
        let root = std::env::temp_dir().join(format!("repterm-dot-{}", std::process::id()));
        let cwd = root.join("repo");
        std::fs::create_dir_all(cwd.join("sub")).unwrap();
        let parent = resolve_path("../../etc/passwd", &cwd.join("sub"), None).unwrap();
        assert!(!is_within(&parent, &cwd));
        assert!(!is_within(Path::new("/etc/passwd"), &cwd));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn edit_file_requires_unique_match_and_valid_utf8() {
        let root = std::env::temp_dir().join(format!("repterm-edit-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("f.txt");
        std::fs::write(&path, "one\none\n").unwrap();
        let call = ToolCall {
            id: "c1".into(),
            name: "edit_file".into(),
            arguments: r#"{"path":"f.txt","old":"one","new":"two","reason":"cambiar"}"#.into(),
        };
        assert!(execute(&call, &root, None).is_err());
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert!(execute(&call, &root, None).is_err());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn write_file_rejects_content_over_limit() {
        let root = std::env::temp_dir().join(format!("repterm-write-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let args = format!(
            r#"{{"path":"big.txt","content":"{}","reason":"test"}}"#,
            "x".repeat(MAX_WRITE_BYTES + 1)
        );
        let call = ToolCall {
            id: "c2".into(),
            name: "write_file".into(),
            arguments: args,
        };
        assert!(execute(&call, &root, None).is_err());
        assert!(!root.join("big.txt").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn valid_write_and_read_roundtrip() {
        let root = std::env::temp_dir().join(format!("repterm-round-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let write = ToolCall {
            id: "w".into(),
            name: "write_file".into(),
            arguments: r#"{"path":"sub/a.txt","content":"hello","reason":"create"}"#.into(),
        };
        let outcome = execute(&write, &root, None).unwrap();
        assert_eq!(outcome.changed_path.as_deref(), Some("sub/a.txt"));
        let read = ToolCall {
            id: "r".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"sub/a.txt","reason":"read"}"#.into(),
        };
        assert!(execute(&read, &root, None)
            .unwrap()
            .content
            .contains("hello"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tool_definitions_cover_four_names_and_require_reason() {
        let defs = tool_definitions();
        assert_eq!(defs.len(), 4);
        let names: Vec<_> = defs
            .iter()
            .map(|d| d["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["list_dir", "read_file", "write_file", "edit_file"]
        );
        for def in &defs {
            let required = def["function"]["parameters"]["required"]
                .as_array()
                .unwrap();
            assert!(required.iter().any(|v| v == "reason"));
        }
    }
}
