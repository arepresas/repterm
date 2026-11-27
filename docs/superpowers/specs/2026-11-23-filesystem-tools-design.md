# File tools for the AI agent in repterm — design

## Intent and why

The user wants the repterm agent to stop limiting itself to proposing commands and to be able to work directly on the open project: read files, create new ones, and modify existing ones. The condition they set is a scope limit, not a general permission: "I only want it to be able to access the current directory; outside of it, it must ask for permission explicitly".

When asked how that permission should work, they chose a detailed dialog with buttons: show the tool, the path, the tab's directory, and the reason, and offer allow once, always allow that path, or reject. They also accepted the assumptions confirmed in the conversation: no confirmation inside the directory; "always allow" applies only to the current session; on rejection, the model receives a clear error and can continue the conversation.

This spec extends the agent mode already implemented in `docs/superpowers/specs/2026-11-22-ai-agent-mode-design.md`. It keeps its rule that the model **does not execute commands on its own**.

## Goals

- Give the model four file tools: `list_dir`, `read_file`, `write_file`, and `edit_file`.
- Resolve all paths relative to the active tab's cwd; freely allow those that fall inside that directory.
- Request explicit permission through a detailed modal dialog for any operation whose canonicalized path falls outside the cwd, including symlinks that point outside.
- Support streaming tool calls with LiteLLM's OpenAI-compatible API, chaining multiple tool rounds until the model gives a final response.
- Remember paths authorized with "always allow" for the session, without writing that list to disk.
- Report created or modified files in the chat so the user can review them with the existing changes panel.

## Non-goals

- Automatic execution of shell commands; `!command` remains a user-initiated action in the PTY.
- Deleting, renaming, moving, or changing file permissions; git operations (stage, commit, checkout); network access beyond LiteLLM itself.
- Persisting "always" permissions across repterm restarts, or offering an interface to revoke the session list: it resets when the app closes.
- Operating-system-level isolation (containers, sandbox, seccomp): the boundary is a path check in the application.
- Diff previews inside the permission dialog; deferred to a later version.
- Multiple simultaneous permission dialogs: they are queued and shown one at a time.
- Including the `README.md` with the Oh My Zsh section in this change; the user left it modified but uncommitted.

## Constraints

- Rust 2021, rustc/cargo 1.99.1, eframe/egui 0.31, vendored `egui_term`: do not touch `vendor/egui_term`.
- No tokio: the HTTP client already uses `std::thread` + `mpsc` in `src/llm.rs:127-223`; waiting for permission must be UI state, not a thread block.
- Follow existing patterns: modal windows in `src/app.rs` (`show_settings`, `show_picker`), `active_cwd()` in `src/app.rs:883-887`, `expand_home()` in `src/gitinfo.rs:48-60`, and the save format in `src/settings.rs`.
- The API key must never appear in logs or in the conversation; tool messages do not include it.
- The working tree has an unrelated uncommitted change (`README.md`, Oh My Zsh); any commit for this work must add only its own files.

## Decisions made

1. **Tool protocol.** OpenAI-compatible requests with `tools: [{type:"function", function:{name,description,parameters}}]` and `tool_choice: "auto"`. `assistant` messages may carry `tool_calls: [{id,type:"function",function:{name,arguments}}]`; results are returned as `role:"tool", tool_call_id, content`. All definitions and chunk parsing live alongside the client in `src/llm.rs` unless the implementer decides to extract a dedicated module.

2. **Tools and their arguments.** `list_dir(path)`, `read_file(path)`, `write_file(path, content)`, and `edit_file(path, old, new)`. All also require `reason: string`, a short phrase shown verbatim in the dialog. There is no delete tool. `edit_file` requires `old` to appear exactly once; if it is absent or appears multiple times, it returns an error to the model without writing anything.

3. **Path resolution.** `~` is expanded with `expand_home`. Relative paths are resolved against the active tab's cwd. `canonicalize` is used whenever possible; for files that do not exist yet, the nearest existing ancestor is canonicalized and the rest is rebuilt. The resulting path must start with the canonicalized cwd. A `..`, an absolute path, or a symlink that escapes the directory falls outside and triggers permission. With no active tab, the tool returns an error and opens no dialog, just like `command_target(None)`.

**User-confirmed decision:** the cwd and tab are fixed when the turn starts. All calls in all rounds of that turn use the same canonicalized cwd and the same `tab_id`, even if the user runs `cd` or switches tabs in the meantime. The dialog always shows that fixed cwd. This prevents the "inside the directory" permission from being widened mid-request.

4. **Permission policy.** Inside the cwd: direct execution, no dialog. Outside: a request is queued with the operation, canonicalized path, cwd, and reason. The dialog offers "Allow once", "Always allow this path" (session-only key per operation + canonicalized path pair), and "Reject". `Escape` and closing the window count as rejection. Rejection is returned to the model as tool content indicating the user denied it; the model can continue.

5. **Tool loop.** When a stream ends with `finish_reason = "tool_calls"`, the turn does not close: the tools run in order (requesting permission when needed), the `assistant` message with those tool calls and the `tool` messages with their results are added, and a new request is sent with the same refreshed context. Limit of 8 rounds per turn — our decision — to avoid runaway loops; the user is informed when it is reached. Cancelling during any round stops the loop and keeps what has already been written, just like the current streaming cancellation.

6. **Streaming and parser.** `choices[0].delta.tool_calls` deltas are grouped by `index`: `id` and `function.name` usually arrive in the first chunk and `function.arguments` is concatenated. `finish_reason: "tool_calls"` arrives before `data: [DONE]`. `parse_sse_frames` is reused; `parse_delta` is extended without breaking the current content tests. The 20-message truncation in `build_messages` must not separate a tool call from its result.

7. **Dialog (option C chosen).** Non-resizable modal `Window` of about 420×220 px, with rows `Tool`, `Path`, `Tab directory`, and `Reason`, and three horizontal buttons. If several requests are queued, `1 of N` is shown. The violet accent color and button style are the ones already used in settings. Keyboard precedence: permission > settings > chat > tabs > terminal. While a permission is pending, the composer and send button are disabled; the chat header shows an amber indicator with the number of rounds in progress.

8. **Writing and limits.** `write_file` creates any missing parent directories and overwrites the file; when the destination is outside the cwd, the dialog says so explicitly. To avoid UI stalls, `read_file` caps output at 64 KiB and reports truncation, and `write_file` rejects content over 256 KiB with an error the model can understand. These three numbers are our decision, not the user's.

9. **Traceability.** After each successful `write_file` or `edit_file`, a short note such as "Modified file: `src/foo.rs`" is added to the chat. No full diff is shown: that is what the existing changes panel is for. Tool calls and their results count toward the 20-message history limit.

10. **System prompt.** The current prompt, which today forbids claiming that tools exist, is updated to describe the four available operations, the scope rule, and the requirement to include `reason`. It is not exposed as a setting.

11. **Results in chat.** The full content goes to the model in the `tool` message, but the UI shows a one-line summary (`list_dir: 42 entries`, `read_file: src/foo.rs, 3.1 KiB`) with a control to expand the full text. Tool messages are not drawn with the "Insert into terminal" buttons of code blocks. Our decision.

12. **`list_dir`.** Single level, non-recursive; at most 200 sorted entries, with `/` for directories and the size for files; if there are more, the truncation is indicated. Our decision.

13. **Binary files.** `read_file` and `edit_file` require valid UTF-8: if it is not, they return a clear error to the model, without `from_utf8_lossy`. `edit_file` applies the same 256 KiB limit as `write_file`, checked via metadata before reading. Our decision.

14. **Partial denial.** If a call is rejected, the rest of that round stops: results for what has already run are returned along with an explicit error for the denied call; subsequent calls are not executed. Each dialog answers only its own call. Our decision.

15. **Closing the chat.** Pressing × in the header during a round cancels the loop and any pending permission, with the same effect as rejecting. No permission dialog is shown while the chat is hidden, and no new messages are accepted while a round is in progress. Our decision.

## Interface (designer brief)

- The permission dialog is a new surface, distinct from the settings modal and the branch picker; it appears only when the model requests an operation outside the cwd.
- Body with four label/value rows in monospace for the path and cwd; the reason in normal text. Buttons aligned to the right, with "Reject" as the safe default action when pressing `Escape`.
- The conversation remains the same chat view; tool results appear as understated messages, not user bubbles, and the amber indicator disappears when the loop ends.

## Left to the implementer

- Exact widget layout, spacing, and button text are set by the UI brief; this spec only fixes behavior.
- Where the tool JSON definitions live and whether to extract `src/tools.rs` to avoid bloating `src/llm.rs` or `src/app.rs`; either approach is fine as long as write scopes do not overlap.
- How the tool-loop state machine's state is structured internally, as long as the UI never blocks waiting for the user and `cancel_chat` clears everything.
- Mandatory unit tests: cwd/tab pinning at turn start, path containment (including `..`, absolute path, and escaping symlink), permission decision and session-only list, aggregation of SSE-fragmented tool calls, loop transitions (execute → permission → result → new request), rejection/cancellation (including mid-round denial), size limits, invalid UTF-8, and absence of an active tab.
- Live checking against the user's LiteLLM proxy is not executable in the development environment; it will be documented as a limitation.

## Known risks

- The model can read secrets inside the cwd (for example `.env`) and reproduce them in the chat. `include_terminal_context = false` does not cover this case; the trust boundary is the directory the user chooses as cwd.
- "Always allow" only remembers the session: the questions return after a restart. That is the trade-off for not persisting authorizations to disk.
- The LiteLLM platform may vary in how it propagates tool-call deltas; the parser must handle partial chunks and not assume the name and identifier arrive complete in the first chunk.
- A persistent model could chain many rounds; the limit of 8 and the cancel button contain the risk.
- Reading or writing files on the UI thread can cause micro-freezes; the size limits reduce them, but a very large README will still be costly.
