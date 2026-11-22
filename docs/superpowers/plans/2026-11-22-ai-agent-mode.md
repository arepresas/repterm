# Modo agente IA (LiteLLM) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Añadir a repterm un modo chat con un modelo servido por LiteLLM, alternable desde el terminal con `Ctrl+Shift+Enter` o con `!`, que conversa con contexto del repo y propone comandos sin ejecutarlos automáticamente.

**Architecture:** `settings.rs` persiste URL/clave/modelo; `llm.rs` encapsula el cliente HTTP compatible con OpenAI y el parser SSE sobre un hilo `std::thread` + `mpsc` (sin tokio); `chat_view.rs` convierte markdown en `LayoutJob`; `app.rs` posee el estado del chat y decide qué superficie (terminal, chat o ajustes) recibe el foco y los eventos.

**Tech Stack:** Rust 2021, eframe/egui 0.31, egui_term vendorizado, ureq 3.5 (blocking + rustls), serde/serde_json, pulldown-cmark 0.13.

**Spec:** `docs/superpowers/specs/2026-11-22-ai-agent-mode-design.md`

## Global Constraints

- Rust 2021; rustc/cargo 1.99.1; eframe/egui 0.31; no añadir tokio ni modificar `vendor/egui_term`.
- `ureq = { version = "3.5", features = ["json"] }`; el streaming se consume desde un hilo con `std::sync::mpsc`, nunca en el hilo de la UI.
- Archivo de ajustes en `$XDG_CONFIG_HOME/repterm/settings.json` o `~/.config/repterm/settings.json`, permisos `0600` en Unix; la clave no se escribe en logs ni en la conversación.
- Base URL por defecto `http://localhost:4000/v1`; endpoint de modelos `GET {base_url}/models`; chat `POST {base_url}/chat/completions` con `stream: true`.
- Contexto: system message refrescado cada turno + últimos 20 mensajes como máximo; `include_terminal_context = false` elimina últimas 120 líneas **y** selección.
- Una conversación global en memoria; comandos dirigidos a la pestaña activa; aviso si no hay pestaña activa.
- **Precondición:** el árbol de trabajo debe estar limpio antes del primer commit; si siguen los cambios sin commitear de la integración git, confirmar con el usuario cómo agruparlos.

## Review Focus

- `!` escrito a mitad de línea debe llegar al shell (expansión de historial): Task 3 prueba `is_bang_text_event` junto al flag `clean_line`.
- SSE partido en varios chunks, `[DONE]` y frames de error: Task 2 prueba `parse_sse_frames` y `parse_delta`.
- Ajustes ausentes o corruptos: Task 1 prueba `load_from` con archivo inexistente y JSON inválido.
- Privacidad con `include_terminal_context = false`: Task 3 prueba que `build_context` omite selección y líneas.
- Sin pestaña activa al enviar `!cmd`: Task 3 prueba `command_target(None)`; la UI muestra aviso y no escribe nada.
- Bloqueos de código en la respuesta: Task 4 prueba que `markdown_layout` conserva el texto del código en monospace.

---

### Task 1: Dependencias y ajustes persistentes

**Files:**
- Modify: `Cargo.toml`, `src/main.rs` (añadir `mod settings;`)
- Create: `src/settings.rs` (con tests inline)

**Interfaces:**
- Consumes: nada.
- Produces: `settings::Settings { pub base_url: String, pub api_key: String, pub model: String, pub include_terminal_context: bool }`; `impl Default`; `pub fn settings_path() -> PathBuf`; `pub fn load_from(path: &Path) -> Settings`; `pub fn save_to(&self, path: &Path) -> std::io::Result<()>`; `pub fn load() -> Settings`; `pub fn save(&self) -> std::io::Result<()>`.

- [ ] **Step 1: Añadir dependencias y el módulo**

En `Cargo.toml`, bajo `[dependencies]`: `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`, `ureq = { version = "3.5", features = ["json"] }`, `pulldown-cmark = "0.13"`. En `src/main.rs`: `mod settings;`.

Run: `cargo check`
Expected: compila (aunque `settings.rs` aún no exista: créalo vacío en este paso).

- [ ] **Step 2: Escribir tests que fallan en `src/settings.rs`**

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

- [ ] **Step 3: Verificar que fallan**

Run: `cargo test settings::tests`
Expected: FAIL, `Settings`/`load_from` no definidos.

- [ ] **Step 4: Implementar `settings.rs`**

Deriva `Serialize, Deserialize, Debug, Clone, PartialEq` en `Settings`; `Default` con los valores de arriba. `settings_path()` usa `XDG_CONFIG_HOME` si existe, si no `HOME/.config`. `save_to` crea el directorio padre, usa `OpenOptions::new().write(true).create(true).truncate(true).mode(0o600)` bajo `cfg(unix)` y `serde_json::to_writer_pretty`. `load_from` lee con `serde_json::from_reader` y devuelve `Default` si falta o falla. `load()`/`save()` delegan en `settings_path()`. Ningún `log` incluye `api_key`.

- [ ] **Step 5: Verificar que pasan**

Run: `cargo test settings::tests`
Expected: 3 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/main.rs src/settings.rs
git commit -m "feat: add LiteLLM settings persistence"
```

### Task 2: Cliente LiteLLM y parser SSE

**Files:**
- Modify: `src/main.rs` (añadir `mod llm;`)
- Create: `src/llm.rs` (con tests inline)

**Interfaces:**
- Consumes: `settings::Settings` (Task 1) solo en la capa de UI.
- Produces: `llm::Role { System, User, Assistant }` (serde lowercase); `llm::ChatMessage { pub role: Role, pub content: String }` con `fn system(text: impl Into<String>)`, `fn user(...)`, `fn assistant(...)`; `llm::StreamUpdate { Delta(String), Done, Error(String) }`; `llm::LlmClient { pub base_url: String, pub api_key: String, pub cancel: Arc<AtomicBool> }`; `pub const SYSTEM_PROMPT: &str`; `pub fn parse_sse_frames(input: &str) -> (Vec<String>, String)`; `pub fn parse_delta(frame: &str) -> Result<Option<String>, String>` (`Ok(None)` = `[DONE]`); `pub fn build_messages(context: &str, history: &[ChatMessage]) -> Vec<ChatMessage>`; `impl LlmClient { pub fn models(&self) -> Result<Vec<String>, String>; pub fn stream(&self, model: &str, messages: &[ChatMessage], tx: &Sender<StreamUpdate>) -> Result<(), String>; }`.

- [ ] **Step 1: Tests del parser y del historial**

```rust
#[test]
fn parse_sse_frames_handles_split_and_keeps_remainder() {
    let (frames, rest) = parse_sse_frames("data: {\"a\":1}\n\ndata: {\"b\":2}\n\nda");
    assert_eq!(frames, vec!["{\"a\":1}", "{\"b\":2}"]);
    assert_eq!(rest, "da");
}

#[test]
fn parse_delta_extracts_text_done_and_errors() {
    assert_eq!(parse_delta("{\"choices\":[{\"delta\":{\"content\":\"hola\"}}]}").unwrap(), Some("hola".into()));
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

- [ ] **Step 2: Verificar que fallan**

Run: `cargo test llm::tests`
Expected: FAIL, módulo no definido.

- [ ] **Step 3: Implementar `llm.rs`**

`SYSTEM_PROMPT`: asistente dentro de un terminal, sin herramientas, comandos en bloques vallados, respuestas concisas. `parse_sse_frames` separa por `\n\n`, extrae líneas `data:` y conserva el resto. `parse_delta` usa `serde_json::Value`: si el frame es `[DONE]` devuelve `None`; si tiene `error.message` devuelve `Err`; si no `choices[0].delta.content` devuelve `Ok(None)`. `build_messages` construye un `ChatMessage::system` con `SYSTEM_PROMPT` + `\n\nCONTEXTO:\n{context}` y añade los últimos 20 mensajes no system. `models()` hace GET con `Authorization: Bearer` y parsea `data[].id`. `stream()` hace POST con JSON `{model, stream: true, messages}`, lee el cuerpo en trozos de 4096 bytes con `Read`, acumula en buffer, llama a `parse_sse_frames`, envía `Delta`/`Done`/`Error` por `tx` y comprueba `cancel.load(Ordering::Relaxed)` en cada iteración.

- [ ] **Step 4: Verificar que pasan**

Run: `cargo test llm::tests`
Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs src/llm.rs
git commit -m "feat: add LiteLLM streaming client and SSE parser"
```

### Task 3: Estado del chat, cambio de modo y prefijo `!`

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `llm::{ChatMessage, LlmClient, StreamUpdate, build_messages}`; `settings::Settings`.
- Produces en `app.rs`: `struct ChatState { pub messages: Vec<ChatMessage>, pub input: String, pub partial: String, pub error: Option<String>, pub cancelled: bool, pub cancel: Arc<AtomicBool>, pub rx: Option<Receiver<StreamUpdate>> }`; `impl ChatState { fn new() -> Self }`; `pub fn bang_command(input: &str) -> Option<&str>`; `fn is_bang_text_event(event: &egui::Event) -> bool`; `fn command_target(active: Option<u64>) -> Result<u64, ()>`; `fn build_context(repo: Option<&RepoStatus>, cwd: Option<&str>, selection: Option<&str>, lines: &[String], include_terminal: bool) -> String`; campos nuevos de `App`: `chat_open: bool`, `chat: ChatState`, `settings: Settings`, `settings_open: bool`, `line_clean: HashMap<u64, bool>`.

- [ ] **Step 1: Tests puros en `app.rs`**

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
    let lines = vec!["SECRETO_LINEA".to_owned()];
    let full = build_context(None, Some("/repo"), Some("SECRETO_SELECCION"), &lines, true);
    let redacted = build_context(None, Some("/repo"), Some("SECRETO_SELECCION"), &lines, false);
    assert!(full.contains("SECRETO_LINEA") && full.contains("SECRETO_SELECCION"));
    assert!(redacted.contains("/repo"));
    assert!(!redacted.contains("SECRETO_LINEA") && !redacted.contains("SECRETO_SELECCION"));
}
```

- [ ] **Step 2: Verificar que fallan**

Run: `cargo test app::tests`
Expected: FAIL, funciones no definidas.

- [ ] **Step 3: Añadir el estado a `App`**

En `App::new()`: cargar `settings::load()`, inicializar `line_clean` vacío, `chat_open = false`, `chat = ChatState::new()` y `settings_open = false`.

- [ ] **Step 4: Implementar las funciones puras**

Implementar `bang_command`, `is_bang_text_event`, `command_target` y `build_context` con las firmas anteriores. `build_context` incluye siempre cwd/rama/lista de `RepoStatus.changes` (ruta, staged, +/−) y solo incluye selección y líneas si `include_terminal` es true; trunca el bloque de terminal a un máximo de 8000 caracteres.

- [ ] **Step 5: Cablear teclado y eventos en `App::update`**

Antes de los paneles: si `settings_open` activo no procesar nada más; si no, consumir `Ctrl+Shift+Enter` con `i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::Enter)` y alternar `chat_open`. En modo terminal, si `line_clean` de la pestaña activa es true, retirar de `ctx.input_mut(|i| &mut i.events)` el primer evento que cumpla `is_bang_text_event`; marcar `line_clean=false` y abrir el chat. Actualizar `line_clean` para la pestaña activa: `true` al detectar `Event::Key { key: Enter, pressed: true, .. }`, `false` al detectar texto imprimible distinto de `!`. Al añadir/quitar pestañas, sincronizar el mapa.

- [ ] **Step 6: Enviar y sondear respuestas**

`send_chat(&mut self, ctx)`: construir mensajes con `build_messages(&context, &self.chat.messages)`, clonar `LlmClient` desde `settings`, crear canal, `Arc::new(AtomicBool::new(false))`, marcar `streaming` implícito con `rx.is_some()`, limpiar `partial` y `error`, lanzar `std::thread::spawn` que llama a `client.stream(...)`. `poll_chat(&mut self, ctx)`: drenar `rx` con `try_recv`, acumular `Delta` en `partial`, `Done` empuja `ChatMessage::assistant(partial)` y cierra el canal, `Error` guarda el mensaje y cierra; llamar `ctx.request_repaint()` mientras haya stream activo. Cancelar: poner el flag en true, conservar `partial` como mensaje marcado `cancelled = true` y reabrir el compositor.

- [ ] **Step 7: Escribir en el PTY al volver al terminal**

Si el compositor envía texto que empieza por `!`: `bang_command` extrae el comando; `command_target(self.tabs.active())` decide; si `Ok(id)`, `self.tabs.tab_mut(id).unwrap().backend.process_command(BackendCommand::Write(format!("{cmd}\n").into_bytes()))`, añadir eco local, `chat_open=false` y enfocar el terminal. Si `Err`, mostrar aviso en el chat y no escribir.

- [ ] **Step 8: Verificar**

Run: `cargo test app::tests && cargo check`
Expected: 4 tests PASS; compila.

- [ ] **Step 9: Commit**

```bash
git add src/app.rs
git commit -m "feat: add chat mode state and bang toggle"
```

### Task 4: Vista de chat y markdown

**Files:**
- Create: `src/chat_view.rs` (con tests inline)
- Modify: `src/main.rs` (añadir `mod chat_view;`), `src/app.rs` (método `show_chat`)

**Interfaces:**
- Consumes: `app::ChatState` y `llm::ChatMessage`.
- Produces: `chat_view::CHAT_ACCENT: Color32 = (116,93,215)`; `chat_view::markdown_layout(text: &str, visuals: &egui::Visuals) -> egui::text::LayoutJob`.

- [ ] **Step 1: Test del layout markdown**

```rust
#[test]
fn markdown_layout_keeps_inline_and_fenced_code_as_monospace() {
    let job = markdown_layout("texto `a` y\n```\ngit status\n```", &egui::Visuals::dark());
    assert!(job.text.contains("git status"));
    assert!(job.sections.iter().any(|s| s.format.font_id == egui::FontId::monospace(12.0)));
}
```

- [ ] **Step 2: Verificar que falla**

Run: `cargo test chat_view::tests`
Expected: FAIL, módulo no definido.

- [ ] **Step 3: Implementar `markdown_layout`**

Usa `pulldown_cmark::Parser` (`Text`, `Code`, `Start(Tag::CodeBlock)`, `End`) y construye un `LayoutJob`: texto normal con `visuals.text_color`, código en `FontId::monospace(12.0)` con fondo `visuals.weak_bg_fill`; conserva los saltos de línea.

- [ ] **Step 4: Implementar `App::show_chat`**

`CentralPanel` con cabecera ~34 px («AI chat», punto de estado verde/ámbar/rojo, modelo de solo lectura, botón ×, acceso a Ajustes), `ScrollArea::vertical().auto_shrink([false,false])` con burbujas usuario/asistente, cursor de bloque parpadeante en `partial`, eco `!cmd` monoespacio y banner de error con «Reintentar»; abajo `TextEdit` multilinea (44–160 px, monoespacio 13) + botón Enviar deshabilitado si está vacío o hay stream; `Enter` envía, `Shift+Enter` salto de línea; los bloques de código muestran un botón «Insertar en terminal» que escribe el texto sin Enter. Llamar `request_focus()` al entrar en chat y tras cada envío.

- [ ] **Step 5: Verificar**

Run: `cargo test chat_view::tests && cargo check`
Expected: 1 test PASS; compila.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/chat_view.rs src/app.rs
git commit -m "feat: render streaming chat panel with markdown"
```

### Task 5: Modal de ajustes y descubrimiento de modelos

**Files:** Modify `src/app.rs`

**Interfaces:**
- Consumes: `settings::{Settings, load, save}`; `llm::LlmClient::models`.
- Produces: `struct SettingsForm { pub base_url: String, pub api_key: String, pub model: String, pub reveal_key: bool }`; `fn from_settings(&Settings) -> Self`; `fn validate(&self) -> Result<(), &'static str>`; `fn to_settings(&self) -> Settings`.

- [ ] **Step 1: Tests de validación del formulario**

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

- [ ] **Step 2: Verificar que falla**

Run: `cargo test app::tests::settings_form`
Expected: FAIL, `SettingsForm` no definido.

- [ ] **Step 3: Implementar formulario y modal**

`validate` solo exige `base_url` no vacío; `to_settings` conserva `include_terminal_context` de `self.settings`. `Window::new("Ajustes").resizable(false)` de ~460×340: Base URL, API key con `.password(!reveal_key)` y botón de ojo, `ComboBox` de modelos con opción manual, checkbox «Incluir contexto del terminal», Guardar/Cancelar. `Escape` cierra sin guardar; Guardar valida, llama `Settings::save()` y cierra; al guardar recargar `self.settings`. Al abrir ajustes, `self.picker = None`; mientras el modal esté abierto, el resto de `update` no procesa teclado.

- [ ] **Step 4: Cargar modelos de forma asíncrona**

Al abrir ajustes (y al pulsar «↻»), lanzar un hilo con `LlmClient { base_url, api_key, cancel: Arc::new(AtomicBool::new(false)) }.models()` y enviar `Vec<String>` por un nuevo `model_rx`. Mostrar «Cargando modelos…», y en error «Modelos no disponibles» con botón de reintento sin bloquear la entrada manual. Guardar el `model_rx` como campo de `App`.

- [ ] **Step 5: Verificar**

Run: `cargo test app::tests && cargo check`
Expected: tests PASS; compila.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "feat: add LiteLLM settings modal and model discovery"
```

### Task 6: Documentación y verificación final

**Files:** Modify `README.md`

**Interfaces:** Consumes: comportamiento de Tasks 1–5.

- [ ] **Step 1: Documentar los controles**

En Features añadir el modo agente con LiteLLM, contexto del repo, streaming y comandos propuestos. En Controls documentar `Ctrl+Shift+Enter`, `!` en terminal, `!cmd` en el chat, `Enter`/`Shift+Enter`, cancelación del streaming y dónde configurar URL/clave/modelo.

- [ ] **Step 2: Verificación completa**

Run: `cargo fmt --all -- --check && cargo test && cargo clippy --all-targets -- -D warnings`
Expected: sin diff de formato, todos los tests pasan, clippy sin errores.

- [ ] **Step 3: Comprobación manual**

Run: `cargo run` con el proxy LiteLLM disponible y una clave válida configurada desde Ajustes; abrir chat con `Ctrl+Shift+Enter` y con `!`, recibir un stream, cancelarlo, volver con `!git status`, insertar un comando propuesto y verificar que el `!` a mitad de línea llega al shell. Si no hay GUI o clave, documentar qué pasos quedaron sin ejecutar.

- [ ] **Step 4: Revisión del diff**

Run: revisar `git diff` del feature completo con el agente revisor disponible; corregir cualquier finding bloqueante antes del commit final.

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "docs: document AI chat mode"
```
