# AI Agent Mode (LiteLLM) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a chat mode to repterm with a model served by LiteLLM, toggleable from the terminal with `Ctrl+Shift+Enter` or with `!`, that converses with repo context and proposes commands without executing them automatically.

**Architecture:** `settings.rs` persists URL/key/model; `llm.rs` encapsulates the OpenAI-compatible HTTP client and the SSE parser on a `std::thread` + `mpsc` thread (no tokio); `chat_view.rs` converts markdown into `LayoutJob`; `app.rs` owns the chat state and decides which surface (terminal, chat, or settings) receives focus and events.

**Tech Stack:** Rust 2021, eframe/egui 0.31, vendored egui_term, ureq 3.5 (blocking + rustls), serde/serde_json, pulldown-cmark 0.13.

**Spec:** `docs/superpowers/specs/2026-11-22-ai-agent-mode-design.md`

## Global Constraints

- Rust 2021; rustc/cargo 1.99.1; eframe/egui 0.31; do not add tokio or modify `vendor/egui_term`.
- `ureq = { version = "3.5", features = ["json"] }`; streaming is consumed from a thread with `std::sync::mpsc`, never on the UI thread.
- Settings file at `$XDG_CONFIG_HOME/repterm/settings.json` or `~/.config/repterm/settings.json`, permissions `0600` on Unix; the key is never written to logs or the conversation.
- Default base URL `http://localhost:4000/v1`; models endpoint `GET {base_url}/models`; chat `POST {base_url}/chat/completions` with `stream: true`.
- Context: system message refreshed each turn + at most the last 20 messages; `include_terminal_context = false` removes the last 120 lines **and** the selection.
- One global in-memory conversation; commands targeted at the active tab; warning if there is no active tab.
- **Precondition:** the working tree must be clean before the first commit; if uncommitted changes from the git integration remain, confirm with the user how to group them.

## Review Focus

- `!` typed mid-line must reach the shell (history expansion): Task 3 tests `is_bang_text_event` alongside the `clean_line` flag.
- SSE split across multiple chunks, `[DONE]`, and error frames: Task 2 tests `parse_sse_frames` and `parse_delta`.
- Missing or corrupt settings: Task 1 tests `load_from` with a nonexistent file and invalid JSON.
- Privacy with `include_terminal_context = false`: Task 3 tests that `build_context` omits the selection and lines.
- No active tab when sending `!cmd`: Task 3 tests `command_target(None)`; the UI shows a warning and writes nothing.
- Code blocks in the response: Task 4 tests that `markdown_layout` preserves code text in monospace.

---

### Task 1: Dependencies and Persistent Settings

**Files:**
- Modify: `Cargo.toml`, `src/main.rs` (add `mod settings;`)
- Create: `src/settings.rs` (with inline tests)

**Interfaces:**
- Consumes: nothing.
- Produces: `settings::Settings { pub base_url: String, pub api_key: String, pub model: String, pub include_terminal_context: bool }`; `impl Default`; `pub fn settings_path() -> PathBuf`; `pub fn load_from(path: &Path) -> Settings`; `pub fn save_to(&self, path: &Path) -> std::io::Result<()>`; `pub fn load() -> Settings`; `pub fn save(&self) -> std::io::Result<()>`.

- [ ] **Step 1: Add dependencies and the module**

In `Cargo.toml`, under `[dependencies]`: `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`, `ureq = { version = "3.5", features = ["json"] }`, `pulldown-cmark = "0.13"`. In `src/main.rs`: `mod settings;`.

Run: `cargo check`
Expected: compiles (even though `settings.rs` does not exist yet: create it empty in this step).

- [ ] **Step 2: Write failing tests in `src/settings.rs`**

```rust
#[test]
fn defaults_point_to_local_litellm() {
    let s = Settings::default();
    assert_eq!(s.base_url, "http://localhost:4000/v1");
    assert!(s.model.is_empty());
    assert!(s.include_terminal_context);
}

#[test]
fn missing_or_invalid_file_falls_back_to_defaults() {
    let dir = std::env::temp_dir().join(format!("repterm-settings-test-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("settings.json");
    assert_eq!(load_from(&path).base_url, Settings::default().base_url);
    std::fs::write(&path, "{not json").unwrap();
    assert_eq!(load_from(&path).api_key, "");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn save_load_roundtrip_preserves_all_fields() {
    let dir = std::env::temp_dir().join(format!("repterm-roundtrip-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("settings.json");
    let s = Settings { base_url: "http://host:4000/v1".into(), api_key: "secret".into(), model: "gpt-4o-mini".into(), include_terminal_context: false };
    s.save_to(&path).unwrap();
    assert_eq!(load_from(&path).api_key, "secret");
    assert!(!load_from(&path).include_terminal_context);
    std::fs::remove_dir_all(&dir).ok();
}
```

- [ ] **Step 3: Verify that they fail**

Run: `cargo test settings::tests`
Expected: FAIL, `Settings`/`load_from` undefined.

- [ ] **Step 4: Implement `settings.rs`**

Derive `Serialize, Deserialize, Debug, Clone, PartialEq` on `Settings`; `Default` with the values above. `settings_path()` uses `XDG_CONFIG_HOME` if set, otherwise `HOME/.config`. `save_to` creates the parent directory, uses `OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)` under `cfg(unix)` and `serde_json::to_writer_pretty`. `load_from` reads with `serde_json::from_reader` and returns `Default` if missing or on failure. `load()`/`save()` delegate to `settings_path()`. No `log` includes `api_key`.

- [ ] **Step 5: Verify that they pass**

Run: `cargo test settings::tests`
Expected: 3 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs src/settings.rs
git commit -m "feat: add LiteLLM settings persistence"
```

### Task 2: LiteLLM Client and SSE Parser

**Files:**
- Modify: `src/main.rs` (add `mod llm;`)
- Create: `src/llm.rs` (with inline tests)

**Interfaces:**
- Consumes: `settings::Settings` (Task 1) only in the UI layer.
- Produces: `llm::Role { System, User, Assistant }` (serde lowercase); `llm::ChatMessage { pub role: Role, pub content: String }` with `fn system(text: impl Into<String>)`, `fn user(...)`, `fn assistant(...)`; `llm::StreamUpdate { Delta(String), Done, Error(String) }`; `llm::LlmClient { pub base_url: String, pub api_key: String, pub cancel: Arc<AtomicBool> }`; `pub const SYSTEM_PROMPT: &str`; `pub fn parse_sse_frames(input: &str) -> (Vec<String>, String)`; `pub fn parse_delta(frame: &str) -> Result<Option<String>, String>` (`Ok(None)` = `[DONE]`); `pub fn build_messages(context: &str, history: &[ChatMessage]) -> Vec<ChatMessage>`; `impl LlmClient { pub fn models(&self) -> Result<Vec<String>, String>; pub fn stream(&self, model: &str, messages: &[ChatMessage], tx: &Sender<StreamUpdate>) -> Result<(), String>; }`.

- [ ] **Step 1: Parser and history tests**

```rust
#[test]
fn parse_sse_frames_handles_split_and_keeps_remainder() {
    let (frames, rest) = parse_sse_frames("data: {\"a\":1}\n\ndata: {\"b\":2}\n\nda");
    assert_eq!(frames, vec!["{\"a\":1}", "{\"b\":2}"]);
    assert_eq!(rest, "da");
}

#[test]
fn parse_delta_extracts_text_done_and_errors() {
    assert_eq!(parse_delta("{\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}").unwrap(), Some("hello".into()));
    assert_eq!(parse_delta("[DONE]").unwrap(), None);
    assert!(parse_delta("{\"error\":{\"message\":\"boom\"}}").is_err());
}

#[test]
fn build_messages_refreshes_context_and_caps_history() {
    let history: Vec<_> = (0..30).map(|i| ChatMessage::user(format!("m{i}"))).collect();
    let msgs = build_messages("CTX", &history);
    assert_eq!(msgs.len(), 21);
    assert!(matches!(msgs[0].role, Role::System));
    assert!(msgs[0].content.contains("CTX"));
    assert_eq!(msgs[1].content, "m10");
}
```

- [ ] **Step 2: Verify that they fail**

Run: `cargo test llm::tests`
Expected: FAIL, module undefined.

- [ ] **Step 3: Implement `llm.rs`**

`SYSTEM_PROMPT`: an assistant inside a terminal, no tools, commands in fenced blocks, concise answers. `parse_sse_frames` splits on `\n\n`, extracts `data:` lines and keeps the remainder. `parse_delta` uses `serde_json::Value`: if the frame is `[DONE]` it returns `None`; if it has `error.message` it returns `Err`; if there is no `choices[0].delta.content` it returns `Ok(None)`. `build_messages` builds a `ChatMessage::system` with `SYSTEM_PROMPT` + `\n\nCONTEXT:\n{context}` and appends the last 20 non-system messages. `models()` performs a GET with `Authorization: Bearer` and parses `data[].id`. `stream()` performs a POST with JSON `{model, stream: true, messages}`, reads the body in 4096-byte chunks with `Read`, accumulates in a buffer, calls `parse_sse_frames`, sends `Delta`/`Done`/`Error` over `tx`, and checks `cancel.load(Ordering::Relaxed)` on every iteration.

- [ ] **Step 4: Verify that they pass**

Run: `cargo test llm::tests`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/llm.rs
git commit -m "feat: add LiteLLM streaming client and SSE parser"
```

### Task 3: Chat State, Mode Switching, and `!` Prefix

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `llm::{ChatMessage, LlmClient, StreamUpdate, build_messages}`; `settings::Settings`.
- Produces in `app.rs`: `struct ChatState { pub messages: Vec<ChatMessage>, pub input: String, pub partial: String, pub error: Option<String>, pub cancelled: bool, pub cancel: Arc<AtomicBool>, pub rx: Option<Receiver<StreamUpdate>> }`; `impl ChatState { fn new() -> Self }`; `pub fn bang_command(input: &str) -> Option<&str>`; `fn is_bang_text_event(event: &egui::Event) -> bool`; `fn command_target(active: Option<u64>) -> Result<u64, ()>`; `fn build_context(repo: Option<&RepoStatus>, cwd: Option<&str>, selection: Option<&str>, lines: &[String], include_terminal: bool) -> String`; new `App` fields: `chat_open: bool`, `chat: ChatState`, `settings: Settings`, `settings_open: bool`, `line_clean: HashMap<u64, bool>`.

- [ ] **Step 1: Pure tests in `app.rs`**

```rust
#[test]
fn bang_command_strips_prefix_and_ignores_bare_bang() {
    assert_eq!(bang_command("!git status"), Some("git status"));
    assert_eq!(bang_command("!"), None);
    assert_eq!(bang_command("git status"), None);
}

#[test]
fn bang_is_text_event_only() {
    assert!(is_bang_text_event(&egui::Event::Text("!".into())));
    assert!(!is_bang_text_event(&egui::Event::Text("a".into())));
    assert!(!is_bang_text_event(&egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }));
}

#[test]
fn command_target_requires_active_tab() {
    assert_eq!(command_target(Some(7)), Ok(7));
    assert_eq!(command_target(None), Err(()));
}

#[test]
fn context_respects_privacy_switch() {
    let lines = vec!["SECRET_LINE".to_owned()];
    let full = build_context(None, Some("/repo"), Some("SECRET_SELECTION"), &lines, true);
    let redacted = build_context(None, Some("/repo"), Some("SECRET_SELECTION"), &lines, false);
    assert!(full.contains("SECRET_LINE") && full.contains("SECRET_SELECTION"));
    assert!(redacted.contains("/repo"));
    assert!(!redacted.contains("SECRET_LINE") && !redacted.contains("SECRET_SELECTION"));
}
```

- [ ] **Step 2: Verify that they fail**

Run: `cargo test app::tests`
Expected: FAIL, functions undefined.

- [ ] **Step 3: Add the state to `App`**

In `App::new()`: load `settings::load()`, initialize `line_clean` empty, `chat_open = false`, `chat = ChatState::new()`, and `settings_open = false`.

- [ ] **Step 4: Implement the pure functions**

Implement `bang_command`, `is_bang_text_event`, `command_target`, and `build_context` with the signatures above. `build_context` always includes cwd/branch/the `RepoStatus.changes` list (path, staged, +/−) and includes the selection and lines only if `include_terminal` is true; truncates the terminal block to a maximum of 8000 characters.

- [ ] **Step 5: Wire up keyboard and events in `App::update`**

Before the panels: if `settings_open` is active, process nothing else; otherwise, consume `Ctrl+Shift+Enter` with `i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Enter)` and toggle `chat_open`. In terminal mode, if `line_clean` for the active tab is true, remove from `ctx.input_mut(|i| &mut i.events)` the first event matching `is_bang_text_event`; set `line_clean=false` and open the chat. Update `line_clean` for the active tab: `true` when detecting `Event::Key { key: Enter, pressed: true, .. }`, `false` when detecting printable text other than `!`. When adding/removing tabs, sync the map.

- [ ] **Step 6: Send and poll responses**

`send_chat(&mut self, ctx)`: build messages with `build_messages(&context, &self.chat.messages)`, clone `LlmClient` from `settings`, create a channel, `Arc::new(AtomicBool::new(false))`, mark implicit `streaming` with `rx.is_some()`, clear `partial` and `error`, spawn `std::thread::spawn` that calls `client.stream(...)`. `poll_chat(&mut self, ctx)`: drain `rx` with `try_recv`, accumulate `Delta` into `partial`, `Done` pushes `ChatMessage::assistant(partial)` and closes the channel, `Error` stores the message and closes; call `ctx.request_repaint()` while a stream is active. Cancel: set the flag to true, keep `partial` as a message marked `cancelled = true`, and reopen the composer.

- [ ] **Step 7: Write to the PTY when returning to the terminal**

If the composer submits text starting with `!`: `bang_command` extracts the command; `command_target(self.tabs.active())` decides; if `Ok(id)`, `self.tabs.tab_mut(id).unwrap().backend.process_command(BackendCommand::Write(format!("{cmd}\n").into_bytes()))`, add a local echo, set `chat_open=false`, and focus the terminal. If `Err`, show a warning in the chat and write nothing.

- [ ] **Step 8: Verify**

Run: `cargo test app::tests && cargo check`
Expected: 4 tests PASS; compiles.

- [ ] **Step 9: Commit**

```bash
git add src/app.rs
git commit -m "feat: add chat mode state and bang toggle"
```

### Task 4: Chat View and Markdown

**Files:**
- Create: `src/chat_view.rs` (with inline tests)
- Modify: `src/main.rs` (add `mod chat_view;`), `src/app.rs` (`show_chat` method)

**Interfaces:**
- Consumes: `app::ChatState` and `llm::ChatMessage`.
- Produces: `chat_view::CHAT_ACCENT: Color32 = (116,93,215)`; `chat_view::markdown_layout(text: &str, visuals: &egui::Visuals) -> egui::text::LayoutJob`.

- [ ] **Step 1: Markdown layout test**

```rust
#[test]
fn markdown_layout_keeps_inline_and_fenced_code_as_monospace() {
    let job = markdown_layout("text `a` and\n```\ngit status\n```", &egui::Visuals::dark());
    assert!(job.text.contains("git status"));
    assert!(job.sections.iter().any(|s| s.format.font_id == egui::FontId::monospace(12.0)));
}
```

- [ ] **Step 2: Verify that it fails**

Run: `cargo test chat_view::tests`
Expected: FAIL, module undefined.

- [ ] **Step 3: Implement `markdown_layout`**

Use `pulldown_cmark::Parser` (`Text`, `Code`, `Start(Tag::CodeBlock)`, `End`) and build a `LayoutJob`: normal text with `visuals.text_color`, code in `FontId::monospace(12.0)` with a `visuals.weak_bg_fill` background; preserve line breaks.

- [ ] **Step 4: Implement `App::show_chat`**

`CentralPanel` with a ~34 px header («AI chat», green/amber/red status dot, read-only model, × button, access to Settings), `ScrollArea::vertical().auto_shrink([false,false])` with user/assistant bubbles, a blinking block cursor on `partial`, a monospace `!cmd` echo, and an error banner with «Retry»; at the bottom a multiline `TextEdit` (44–160 px, monospace 13) + a Send button disabled when empty or while streaming; `Enter` sends, `Shift+Enter` inserts a line break; code blocks show an «Insert into terminal» button that writes the text without Enter. Call `request_focus()` when entering chat and after each send.

- [ ] **Step 5: Verify**

Run: `cargo test chat_view::tests && cargo check`
Expected: 1 test PASS; compiles.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/chat_view.rs src/app.rs
git commit -m "feat: render streaming chat panel with markdown"
```

### Task 5: Settings Modal and Model Discovery

**Files:** Modify `src/app.rs`

**Interfaces:**
- Consumes: `settings::{Settings, load, save}`; `llm::LlmClient::models`.
- Produces: `struct SettingsForm { pub base_url: String, pub api_key: String, pub model: String, pub reveal_key: bool }`; `fn from_settings(&Settings) -> Self`; `fn validate(&self) -> Result<(), &'static str>`; `fn to_settings(&self) -> Settings`.

- [ ] **Step 1: Form validation tests**

```rust
#[test]
fn settings_form_requires_base_url_and_keeps_other_fields() {
    let mut f = SettingsForm { base_url: "".into(), api_key: "k".into(), model: "m".into(), reveal_key: false };
    assert_eq!(f.validate(), Err("base_url required"));
    f.base_url = "http://localhost:4000/v1".into();
    assert_eq!(f.validate(), Ok(()));
    let s = f.to_settings();
    assert_eq!(s.api_key, "k");
    assert_eq!(s.model, "m");
}
```

- [ ] **Step 2: Verify that it fails**

Run: `cargo test app::tests::settings_form`
Expected: FAIL, `SettingsForm` undefined.

- [ ] **Step 3: Implement the form and modal**

`validate` only requires a non-empty `base_url`; `to_settings` preserves `include_terminal_context` from `self.settings`. `Window::new("Settings").resizable(false)` at ~460×340: Base URL, API key with `.password(!reveal_key)` and an eye button, a model `ComboBox` with a manual option, an «Include terminal context» checkbox, Save/Cancel. `Escape` closes without saving; Save validates, calls `Settings::save()`, and closes; on save, reload `self.settings`. When opening settings, `self.picker = None`; while the modal is open, the rest of `update` does not process keyboard input.

- [ ] **Step 4: Load models asynchronously**

When opening settings (and when pressing «↻»), spawn a thread with `LlmClient { base_url, api_key, cancel: Arc::new(AtomicBool::new(false)) }.models()` and send `Vec<String>` over a new `model_rx`. Show «Loading models…», and on error «Models unavailable» with a retry button without blocking manual entry. Store `model_rx` as an `App` field.

- [ ] **Step 5: Verify**

Run: `cargo test app::tests && cargo check`
Expected: tests PASS; compiles.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "feat: add LiteLLM settings modal and model discovery"
```

### Task 6: Documentation and Final Verification

**Files:** Modify `README.md`

**Interfaces:** Consumes: behavior of Tasks 1–5.

- [ ] **Step 1: Document the controls**

In Features, add the agent mode with LiteLLM, repo context, streaming, and proposed commands. In Controls, document `Ctrl+Shift+Enter`, `!` in the terminal, `!cmd` in chat, `Enter`/`Shift+Enter`, stream cancellation, and where to configure URL/key/model.

- [ ] **Step 2: Full verification**

Run: `cargo fmt --all -- --check && cargo test && cargo clippy --all-targets -- -D warnings`
Expected: no formatting diff, all tests pass, clippy without errors.

- [ ] **Step 3: Manual check**

Run: `cargo run` with the LiteLLM proxy available and a valid key configured from Settings; open chat with `Ctrl+Shift+Enter` and with `!`, receive a stream, cancel it, return with `!git status`, insert a proposed command, and verify that `!` mid-line reaches the shell. If there is no GUI or key, document which steps were not executed.

- [ ] **Step 4: Diff review**

Run: review the `git diff` of the complete feature with the available reviewer agent; fix any blocking finding before the final commit.

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "docs: document AI chat mode"
```
