# File tools for the AI agent — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give repterm's chat file tools scoped to the active tab's cwd, with a detailed permission dialog for any path outside that directory.

**Architecture:** `src/tools.rs` centralizes the tool protocol, path resolution/containment, and execution of `list_dir`/`read_file`/`write_file`/`edit_file`. `src/llm.rs` extends the SSE client for fragmented tool calls. `src/app.rs` owns the turn state machine (snapshotted cwd, rounds, permissions, cancellation) and the modal dialog; `src/chat_view.rs` draws result summaries.

**Tech Stack:** Rust 2021, eframe/egui 0.31, ureq 3.5 + serde_json, pulldown-cmark 0.13, `std::thread` + `mpsc`.

**Spec:** `docs/superpowers/specs/2026-11-23-filesystem-tools-design.md`

## Global Constraints

- Rust 2021; rustc/cargo 1.99.1; eframe/egui 0.31; vendored `egui_term`: do not touch `vendor/egui_term`.
- No tokio: the HTTP request stays on a `std::thread` with `mpsc`; waiting for a permission is UI state, never a blocking wait.
- Follow existing patterns: `active_cwd()` (`src/app.rs:883-887`), `expand_home()` (`src/gitinfo.rs:48-60`), the modal windows in `src/app.rs`, and the client in `src/llm.rs:127-223`.
- The API key is never written to logs or the conversation.
- The canonicalized cwd and `tab_id` are snapshotted at turn start and reused across all rounds, even if the user runs `cd` or switches tabs.
- Four tools: `list_dir`, `read_file`, `write_file`, `edit_file`; all require `reason: string`. No deletion.
- Limits: 64 KiB of output for `read_file`, 256 KiB for `write_file` and `edit_file`, 200 entries for `list_dir`, 8 rounds per turn.
- Outside the cwd: dialog with "Allow once", "Allow always this path" (in memory only, per operation+path pair), and "Deny". `Escape`/close = deny. A denial stops the rest of the round.
- `README.md` has unrelated uncommitted changes: **do not add it to any commit in this plan**.

## Review Focus

- A symlink inside the cwd that points outside, or a `..`, must end up in the permission dialog, not in a silent read/write → Task 1, `canonicalized_symlink_outside_cwd_is_not_within`.
- A binary file or an `edit_file` with a non-unique `old` must return an error to the model without writing anything → Task 1, `edit_file_requires_unique_match_and_valid_utf8`.
- Tool calls arriving fragmented across several SSE deltas, with split `id`/`name`, must be reassembled completely → Task 2, `aggregates_split_tool_call_fragments`.
- Trimming to 20 messages must not separate an `assistant` with `tool_calls` from its `tool` messages → Task 2, `build_messages_keeps_tool_call_pairs`.
- If the user denies a call with three more behind it, the remaining three are not executed and the model receives the error → Task 3, `denial_stops_remaining_calls`.
- Closing the chat during a round cancels the loop and discards the pending permission, leaving no orphaned dialogs → Task 3, `cancel_turn_clears_permission`.
- With no active tab or no cwd, the tool fails with a clear message and opens no dialog → Task 3, `new_tool_turn_requires_active_cwd`.

---

### Task 1: Tool module and path policy

**Files:**
- Create: `src/tools.rs` (with inline tests)
- Modify: `src/main.rs` (add `mod tools;`)

**Interfaces:**
- Consumes: `gitinfo::expand_home(text: &str, home: Option<&str>) -> Option<String>`.
- Produces:
  - `pub enum ToolName { ListDir, ReadFile, WriteFile, EditFile }` with `as_str() -> &'static str` and `from_str(&str) -> Option<Self>`.
  - `pub struct ToolCall { pub id: String, pub name: String, pub arguments: String }`.
  - `pub struct ToolOutcome { pub call_id: String, pub content: String, pub changed_path: Option<String> }`.
  - `pub struct PermissionKey { pub tool: ToolName, pub path: PathBuf }` (derives `Clone, Debug, PartialEq, Eq, Hash`).
  - `pub fn tool_definitions() -> Vec<serde_json::Value>`.
  - `pub fn resolve_path(raw: &str, cwd: &Path, home: Option<&str>) -> Result<PathBuf, String>`.
  - `pub fn is_within(path: &Path, cwd: &Path) -> bool`.
  - `pub fn execute(call: &ToolCall, cwd: &Path, home: Option<&str>) -> Result<ToolOutcome, String>`.
  - Constants `pub const MAX_READ_BYTES: usize = 65_536;`, `pub const MAX_WRITE_BYTES: usize = 262_144;`, `pub const MAX_LIST_ENTRIES: usize = 200;`.

- [ ] **Step 1: Write the failing tests in `src/tools.rs`**

```rust
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
    let call = ToolCall { id: "c1".into(), name: "edit_file".into(),
        arguments: r#"{"path":"f.txt","old":"one","new":"two","reason":"change"}"#.into() };
    assert!(execute(&call, &root, None).is_err());
    std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    assert!(execute(&call, &root, None).is_err());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn write_file_rejects_content_over_limit() {
    let root = std::env::temp_dir().join(format!("repterm-write-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let args = format!(r#"{{"path":"big.txt","content":"{}","reason":"test"}}"#, "x".repeat(MAX_WRITE_BYTES + 1));
    let call = ToolCall { id: "c2".into(), name: "write_file".into(), arguments: args };
    assert!(execute(&call, &root, None).is_err());
    assert!(!root.join("big.txt").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn valid_write_and_read_roundtrip() {
    let root = std::env::temp_dir().join(format!("repterm-round-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let write = ToolCall { id: "w".into(), name: "write_file".into(),
        arguments: r#"{"path":"sub/a.txt","content":"hello","reason":"create"}"#.into() };
    let outcome = execute(&write, &root, None).unwrap();
    assert_eq!(outcome.changed_path.as_deref(), Some("sub/a.txt"));
    let read = ToolCall { id: "r".into(), name: "read_file".into(),
        arguments: r#"{"path":"sub/a.txt","reason":"read"}"#.into() };
    assert!(execute(&read, &root, None).unwrap().content.contains("hello"));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn tool_definitions_cover_four_names_and_require_reason() {
    let defs = tool_definitions();
    assert_eq!(defs.len(), 4);
    let names: Vec<_> = defs.iter().map(|d| d["function"]["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["list_dir", "read_file", "write_file", "edit_file"]);
    for def in &defs {
        let required = def["function"]["parameters"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "reason"));
    }
}
```

- [ ] **Step 2: Verify that they fail**

Run: `cargo test tools::tests`
Expected: FAIL because `src/tools.rs` does not exist yet.

- [ ] **Step 3: Create the module with types, constants, and `tool_definitions`**

In `src/tools.rs`, declare the types and constants from Interfaces. `tool_definitions()` returns four objects `{"type":"function","function":{"name":…,"description":…,"parameters":{…}}}`. Each `parameters` is `type:"object"` with `path` and `reason` required; `write_file` adds `content`, `edit_file` adds `old` and `new`, all required. Descriptions are short phrases in English. Do not write the body of `execute` yet.

- [ ] **Step 4: Implement path resolution and containment**

`resolve_path` expands `~` with `expand_home`, joins relative paths to `cwd`, and canonicalizes; if it does not exist, it canonicalizes the nearest existing ancestor and rebuilds the remaining components. `is_within` compares components with `Path::starts_with` on canonicalized paths.

- [ ] **Step 5: Implement `execute`**

Parse `arguments` as `serde_json::Value` and require a non-empty `reason`. `read_file`: strict UTF-8, truncates to `MAX_READ_BYTES` and indicates truncation. `write_file`: rejects more than `MAX_WRITE_BYTES`, creates parents and overwrites; returns `changed_path` relative to `cwd`. `edit_file`: checks metadata before reading, requires valid UTF-8, counts occurrences of `old`, requires exactly one, and returns `changed_path`. `list_dir`: one level, sorts, marks directories with `/`, shows file sizes, and limits to `MAX_LIST_ENTRIES` indicating the truncation. An unknown name returns `Err`.

- [ ] **Step 6: Add the module to the binary**

In `src/main.rs`, add `mod tools;` alongside the existing `mod` declarations.

- [ ] **Step 7: Verify the tests**

Run: `cargo test tools::tests`
Expected: PASS with the six tests above.

- [ ] **Step 8: Commit**

```bash
git add src/tools.rs src/main.rs
git commit -m "feat: add scoped filesystem tools and path policy"
```

---

### Task 2: Tool call protocol in the LiteLLM client

**Files:**
- Modify: `src/llm.rs`

**Interfaces:**
- Consumes: `tools::ToolCall`, `tools::tool_definitions()`.
- Produces:
  - `Role::Tool`.
  - `ChatMessage` with `pub tool_calls: Option<Vec<ToolCall>>` and `pub tool_call_id: Option<String>` (both `skip_serializing_if = "Option::is_none"`), plus `pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self` and `pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self`.
  - `pub struct ToolCallFragment { pub index: usize, pub id: Option<String>, pub name: Option<String>, pub arguments: Option<String> }`.
  - `pub fn parse_tool_call_delta(frame: &str) -> Result<Vec<ToolCallFragment>, String>`.
  - `pub fn aggregate_tool_calls(fragments: &[ToolCallFragment]) -> Vec<ToolCall>`.
  - `pub fn completion_payload(model: &str, messages: &[ChatMessage], tools: &[serde_json::Value]) -> serde_json::Value`.
  - `StreamUpdate::ToolCalls(Vec<ToolCall>)`.
  - `LlmClient::stream(&self, model: &str, messages: &[ChatMessage], tx: &Sender<StreamUpdate>, tools: &[serde_json::Value]) -> Result<(), String>`.
  - `build_messages(context: &str, history: &[ChatMessage]) -> Vec<ChatMessage>` (same signature, new grouping).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn aggregates_split_tool_call_fragments() {
    let frames = [
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_","arguments":""}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"file","arguments":"{\"path\":"}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]}}]}"#,
    ];
    let mut fragments = Vec::new();
    for frame in frames {
        fragments.extend(parse_tool_call_delta(frame).unwrap());
    }
    let calls = aggregate_tool_calls(&fragments);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_1");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments, r#"{"path":"a.txt"}"#);
}

#[test]
fn build_messages_keeps_tool_call_pairs() {
    let history = vec![
        ChatMessage::user("read the file"),
        ChatMessage::assistant_tool_calls(vec![ToolCall { id: "c1".into(), name: "read_file".into(), arguments: "{}".into() }]),
        ChatMessage::tool_result("c1", "content"),
    ];
    let msgs = build_messages("CTX", &history);
    let roles: Vec<_> = msgs.iter().skip(1).map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
}

#[test]
fn completion_payload_includes_tools_when_nonempty() {
    let defs = tool_definitions();
    let payload = completion_payload("m", &[ChatMessage::user("hello")], &defs);
    assert_eq!(payload["stream"], true);
    assert_eq!(payload["tool_choice"], "auto");
    assert_eq!(payload["tools"].as_array().unwrap().len(), 4);
    let empty = completion_payload("m", &[], &[]);
    assert!(empty.get("tools").is_none());
}

#[test]
fn system_prompt_describes_available_tools() {
    assert!(SYSTEM_PROMPT.contains("list_dir"));
    assert!(SYSTEM_PROMPT.contains("read_file"));
    assert!(SYSTEM_PROMPT.contains("reason"));
}
```

- [ ] **Step 2: Verify that they fail**

Run: `cargo test llm::tests`
Expected: FAIL due to symbols and variants not yet defined.

- [ ] **Step 3: Extend the protocol types**

Add `Role::Tool`, the two optional fields and the constructors to `ChatMessage`, and the `StreamUpdate::ToolCalls(Vec<ToolCall>)` variant. Existing constructors leave the new fields as `None`. Do not change the `stream` signature yet.

- [ ] **Step 4: Implement the parser and aggregator**

`parse_tool_call_delta` reads `/choices/0/delta/tool_calls`, extracts `index` and the optional fields `id`, `type`, `function.name`, and `function.arguments`; it ignores frames without tool calls. `aggregate_tool_calls` groups by `index` in order of appearance, keeps the first non-empty `id`/`name`, and concatenates the `arguments` fragments.

- [ ] **Step 5: Implement `completion_payload` and use it in `stream`**

Build the JSON with `model`, `stream: true`, `messages`, and — only if `tools` is non-empty — `tools` and `tool_choice: "auto"`. Change the `stream` signature to accept `tools` and use `completion_payload`. In the loop, accumulate the fragments returned by `parse_tool_call_delta`; on `[DONE]` or EOF, if there are fragments send `StreamUpdate::ToolCalls(aggregate_tool_calls(...))`, otherwise send `Done`. Keep the current behavior for `Delta`, `Error`, and cancellation.

- [ ] **Step 6: Rewrite the system prompt**

Replace the sentence that denies tools with a brief description of the four operations, the rule "only the tab's directory; permission is requested outside it", and the requirement to include `reason` in every call.

- [ ] **Step 7: Adjust `build_messages`**

Group each `assistant` with `tool_calls` and its subsequent `tool` messages as an indivisible unit when applying `HISTORY_LIMIT = 20`. The `system` message is still refreshed on every request.

- [ ] **Step 8: Verify the tests**

Run: `cargo test llm::tests`
Expected: PASS with the four tests above and the three pre-existing ones.

- [ ] **Step 9: Commit**

```bash
git add src/llm.rs
git commit -m "feat: support streamed tool calls in LiteLLM client"
```

---

### Task 3: Turn state, execution, and permissions in the app

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `tools::{ToolCall, ToolOutcome, ToolName, PermissionKey, execute, is_within, tool_definitions}`, `llm::{StreamUpdate, ChatMessage}`.
- Produces:
  - `pub struct PermissionRequest { pub call: ToolCall, pub key: PermissionKey, pub reason: String, pub index: usize, pub total: usize }`.
  - `pub struct ToolTurn { pub tab_id: u64, pub cwd: PathBuf, pub round: usize, pub calls: Vec<ToolCall>, pub outcomes: Vec<ToolOutcome>, pub next: usize, pub permission: Option<PermissionRequest>, pub stopped: bool }`.
  - `pub enum PermissionDecision { AllowOnce, AllowAlways, Deny }`.
  - `pub enum ToolAction { Execute(ToolCall), Ask(PermissionRequest), Finish }`.
  - `ChatState` adds `pub turn: Option<ToolTurn>`.
  - `App` adds `allowed_tools: HashSet<PermissionKey>`.
  - Free functions: `fn new_tool_turn(tab_id: Option<u64>, cwd: Option<&str>) -> Result<ToolTurn, String>`, `fn next_tool_action(turn: &ToolTurn, allowed: &HashSet<PermissionKey>) -> ToolAction`, `fn apply_permission(turn: &mut ToolTurn, allowed: &mut HashSet<PermissionKey>, decision: PermissionDecision)`, `fn clear_tool_turn(chat: &mut ChatState)`.
  - Methods: `fn spawn_chat_request(&mut self, ctx: &egui::Context)`, `fn advance_tools(&mut self, ctx: &egui::Context)`, `fn resolve_permission(&mut self, decision: PermissionDecision, ctx: &egui::Context)`.

- [ ] **Step 1: Write the failing tests in `src/app.rs`**

```rust
#[test]
fn new_tool_turn_requires_active_cwd() {
    assert!(new_tool_turn(None, None).is_err());
    assert!(new_tool_turn(Some(1), None).is_err());
}

#[test]
fn new_tool_turn_snapshots_tab_and_cwd() {
    let root = std::env::temp_dir().join(format!("repterm-turn-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let turn = new_tool_turn(Some(7), root.to_str()).unwrap();
    assert_eq!(turn.tab_id, 7);
    assert_eq!(turn.cwd, std::fs::canonicalize(&root).unwrap());
    assert_eq!(turn.round, 1);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn next_tool_action_asks_outside_and_executes_allowed() {
    let root = std::env::temp_dir().join(format!("repterm-policy-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let cwd = std::fs::canonicalize(&root).unwrap();
    let mut turn = ToolTurn { tab_id: 1, cwd: cwd.clone(), round: 1,
        calls: vec![ToolCall { id: "c1".into(), name: "read_file".into(),
            arguments: r#"{"path":"/etc/hosts","reason":"diagnosis"}"#.into() }],
        outcomes: vec![], next: 0, permission: None, stopped: false };
    let mut allowed = HashSet::new();
    assert!(matches!(next_tool_action(&turn, &allowed), ToolAction::Ask(_)));
    let tool = ToolName::from_str("read_file").unwrap();
    allowed.insert(PermissionKey { tool, path: PathBuf::from("/etc/hosts") });
    assert!(matches!(next_tool_action(&turn, &allowed), ToolAction::Execute(_)));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn denial_stops_remaining_calls() {
    let root = std::env::temp_dir().join(format!("repterm-deny-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let cwd = std::fs::canonicalize(&root).unwrap();
    let call = |id: &str| ToolCall { id: id.into(), name: "read_file".into(),
        arguments: r#"{"path":"/etc/hosts","reason":" diagnosis"}"#.into() };
    let mut turn = ToolTurn { tab_id: 1, cwd, round: 1,
        calls: vec![call("a"), call("b"), call("c")], outcomes: vec![], next: 0,
        permission: Some(PermissionRequest { call: call("a"),
            key: PermissionKey { tool: ToolName::ReadFile, path: PathBuf::from("/etc/hosts") },
            reason: "diagnosis".into(), index: 1, total: 3 }), stopped: false };
    let mut allowed = HashSet::new();
    apply_permission(&mut turn, &mut allowed, PermissionDecision::Deny);
    turn.stopped = true;
    assert!(turn.permission.is_none());
    assert!(allowed.is_empty());
    assert!(matches!(next_tool_action(&turn, &allowed), ToolAction::Finish));
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn cancel_turn_clears_permission() {
    let mut chat = ChatState::new();
    let root = std::env::temp_dir().join(format!("repterm-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let cwd = std::fs::canonicalize(&root).unwrap();
    chat.turn = Some(ToolTurn { tab_id: 1, cwd, round: 1, calls: vec![], outcomes: vec![],
        next: 0, permission: Some(PermissionRequest { call: ToolCall { id: "x".into(),
        name: "read_file".into(), arguments: "{}".into() },
        key: PermissionKey { tool: ToolName::ReadFile, path: PathBuf::from("/etc/hosts") },
        reason: "r".into(), index: 1, total: 1 }), stopped: false });
    clear_tool_turn(&mut chat);
    assert!(chat.turn.is_none());
    std::fs::remove_dir_all(&root).ok();
}
```

- [ ] **Step 2: Verify that they fail**

Run: `cargo test app::tests`
Expected: FAIL due to missing types and functions.

- [ ] **Step 3: Add the types and pure functions**

Define the Interfaces types in `src/app.rs`, add `turn` to `ChatState` and `allowed_tools` to `App` (initialized empty in `new`). Implement `new_tool_turn`, `next_tool_action`, and `apply_permission`. `next_tool_action` returns `Ask` when the canonicalized path is outside the cwd and not in `allowed`; `Execute` otherwise; `Finish` when `next >= calls.len()` or `stopped`. `AllowAlways` inserts the key into `allowed`; `Deny` sets `stopped`, inserts nothing, and clears `permission`.

- [ ] **Step 4: Connect streaming to the turn**

In `send_chat`, before spawning the thread, create the turn with `new_tool_turn(self.tabs.active(), self.active_cwd().as_deref())`; if it fails, show the error and do not send. Store the turn in `chat.turn`. `spawn_chat_request` builds the messages with `build_messages(&self.chat_context(), &self.chat.messages)` and calls `client.stream(..., &tool_definitions())` in the thread, reusing the current `cancel`.

- [ ] **Step 5: Handle `StreamUpdate::ToolCalls` in `poll_chat`**

On receiving tool calls, store them in `turn.calls`, keep `partial` as the assistant text, and do not close the turn. On `Done`: if `turn.calls` is empty, close the turn as today; otherwise, add the `assistant_tool_calls` message, reset `partial`, and call `advance_tools`.

- [ ] **Step 6: Implement `advance_tools` and `resolve_permission`**

`advance_tools` iterates with `next_tool_action`: `Execute` calls `execute`, stores the `ToolOutcome`, and advances; `Ask` stores the `PermissionRequest` and waits; `Finish` adds a `ChatMessage::tool_result` for each outcome (or an explicit error if there was a denial), adds the note "File modified: …" for `changed_path` entries, and decides: if `stopped` or `round >= 8`, it closes the turn with a notice message when appropriate; otherwise, it increments `round` and calls `spawn_chat_request` again. `resolve_permission` applies the decision, clears `permission`, and resumes `advance_tools`.

- [ ] **Step 7: Cancellation and closing**

Implement `clear_tool_turn` to set `chat.turn = None` without touching `allowed_tools`. Extend `cancel_chat` so it clears the turn **before** the current `let Some(rx) = ... else { return; }`, sets `cancelled`, and cancels the shared flag. Make `close_chat` call `cancel_chat` when a turn is in progress.

- [ ] **Step 8: Verify the tests and the build**

Run: `cargo test app::tests`
Expected: PASS with the five tests above and the existing ones.

- [ ] **Step 9: Commit**

```bash
git add src/app.rs
git commit -m "feat: run agent file tools with scoped permissions"
```

---

### Task 4: Permission dialog and results in chat

**Files:**
- Modify: `src/app.rs`
- Modify: `src/chat_view.rs`

**Interfaces:**
- Consumes: `PermissionRequest`, `PermissionDecision`, `ToolOutcome` from Tasks 1-3.
- Produces: `pub fn tool_result_summary(content: &str) -> String` in `src/chat_view.rs`; `fn permission_rows(request: &PermissionRequest) -> [(&'static str, String); 4]` in `src/app.rs`; method `fn show_permission(&mut self, ctx: &egui::Context)`.

- [ ] **Step 1: Write the failing tests**

In `src/chat_view.rs`:

```rust
#[test]
fn tool_result_summary_is_one_line() {
    let summary = tool_result_summary("list_dir: 42 entries\na.txt\nb.txt");
    assert_eq!(summary, "list_dir: 42 entries");
}
```

In `src/app.rs`:

```rust
#[test]
fn permission_rows_show_tool_path_cwd_and_reason() {
    let req = PermissionRequest { call: ToolCall { id: "c1".into(), name: "read_file".into(), arguments: "{}".into() },
        key: PermissionKey { tool: ToolName::ReadFile, path: PathBuf::from("/etc/hosts") },
        reason: "diagnosis".into(), index: 1, total: 2 };
    let rows = permission_rows(&req);
    assert_eq!(rows[0].0, "Tool");
    assert_eq!(rows[0].1, "read_file");
    assert_eq!(rows[1].1, "/etc/hosts");
    assert_eq!(rows[3].1, "diagnosis");
}
```

- [ ] **Step 2: Verify that they fail**

Run: `cargo test tool_result_summary_is_one_line permission_rows_show_tool_path_cwd_and_reason`
Expected: FAIL due to missing functions.

- [ ] **Step 3: Implement the result summary**

`tool_result_summary` returns the first line of the content, truncated to 100 characters, or `tool result` if empty. In `show_chat`, `Role::Tool` messages are drawn with that summary in monospace and a "View" button that expands the full content; no "Insert into terminal" blocks are added to them.

- [ ] **Step 4: Implement `permission_rows` and the dialog**

`permission_rows` returns the four rows `Tool`, `Path`, `Tab directory`, and `Reason`, with the path and cwd in plain text. `show_permission` draws a non-resizable `egui::Window` of 420×220 with those rows, a title `Permission requested (1 of N)`, and three buttons: "Allow once", "Allow always this path", and "Deny". `Escape` and closing the window resolve as `Deny`.

- [ ] **Step 5: Wire up precedence and visual state**

In `update`, when a permission is pending, consume `Escape` before any other shortcut and draw `show_permission` before `show_chat`. In the chat header, show an amber dot with `round N/8` while `chat.turn.is_some()`. Disable the composer and the "Send" button while a turn is in progress, not only while a permission is pending.

- [ ] **Step 6: Verify the tests**

Run: `cargo test`
Expected: PASS with the two new tests and the entire previous suite.

- [ ] **Step 7: Commit**

```bash
git add src/app.rs src/chat_view.rs
git commit -m "feat: add permission dialog and tool result summaries"
```

---

### Task 5: Final verification and wrap-up

**Files:**
- Modify: only if a verification failure appears: `src/tools.rs`, `src/llm.rs`, `src/app.rs`, `src/chat_view.rs`.

**Interfaces:**
- Consumes: all of the above.

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: finishes without errors.

- [ ] **Step 2: Check formatting**

Run: `cargo fmt --all -- --check`
Expected: no output.

- [ ] **Step 3: Full suite**

Run: `cargo test`
Expected: all tests pass, including the new ones in tools, llm, app, and chat_view.

- [ ] **Step 4: Strict lint**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Check the diff and status**

Run: `git status --short && git diff --stat a969321..HEAD`
Expected: `README.md` still appears as modified but is **not** included in the plan's commits; the rest of the changed files are those of this plan.

- [ ] **Step 6: Manual review of the full diff**

Read `git diff a969321..HEAD` looking for: paths that could escape the cwd without permission, tool calls lost when trimming history, permissions persisted by mistake, and error messages that could include the API key. Note any findings and fix them in a single commit if any appear.

- [ ] **Step 7: Fix commit (only if there are changes)**

```bash
git add src/tools.rs src/llm.rs src/app.rs src/chat_view.rs
git commit -m "fix: address final review findings"
```
