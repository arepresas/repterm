# Herramientas de archivos para el agente IA — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Dar al chat de repterm herramientas de archivo limitadas al cwd de la pestaña activa, con un diálogo de permiso detallado para cualquier ruta fuera de ese directorio.

**Architecture:** `src/tools.rs` concentra el protocolo de herramientas, la resolución/cotención de rutas y la ejecución de `list_dir`/`read_file`/`write_file`/`edit_file`. `src/llm.rs` amplía el cliente SSE para tool calls fragmentados. `src/app.rs` posee la máquina de estados del turno (cwd fijado, rondas, permisos, cancelación) y el diálogo modal; `src/chat_view.rs` dibuja resúmenes de resultados.

**Tech Stack:** Rust 2021, eframe/egui 0.31, ureq 3.5 + serde_json, pulldown-cmark 0.13, `std::thread` + `mpsc`.

**Spec:** `docs/superpowers/specs/2026-11-23-filesystem-tools-design.md`

## Global Constraints

- Rust 2021; rustc/cargo 1.99.1; eframe/egui 0.31; `egui_term` vendorizado: no tocar `vendor/egui_term`.
- Sin tokio: la petición HTTP sigue en un `std::thread` con `mpsc`; la espera a un permiso es estado en la UI, nunca un bloqueo.
- Seguir los patrones existentes: `active_cwd()` (`src/app.rs:883-887`), `expand_home()` (`src/gitinfo.rs:48-60`), ventanas modales de `src/app.rs` y el cliente de `src/llm.rs:127-223`.
- La clave de API no se escribe en logs ni en la conversación.
- El cwd canonicalizado y el `tab_id` se fijan al iniciar el turno y se reutilizan en todas las rondas, aunque el usuario haga `cd` o cambie de pestaña.
- Cuatro herramientas: `list_dir`, `read_file`, `write_file`, `edit_file`; todas requieren `reason: string`. No hay borrado.
- Topes: 64 KiB de salida en `read_file`, 256 KiB en `write_file` y `edit_file`, 200 entradas en `list_dir`, 8 rondas por turno.
- Fuera del cwd: diálogo con «Permitir una vez», «Permitir siempre esta ruta» (solo en memoria, por pareja operación+ruta) y «Rechazar». `Escape`/cerrar = rechazar. Una denegación detiene el resto de la ronda.
- `README.md` tiene cambios ajenos sin commitear: **no añadirlo a ningún commit de este plan**.

## Review Focus

- Un symlink dentro del cwd que apunta fuera, o un `..`, debe acabar en el diálogo de permiso, no en lectura/escritura silenciosa → Task 1, `canonicalized_symlink_outside_cwd_is_not_within`.
- Un archivo binario o un `edit_file` con `old` no único debe devolver error al modelo sin escribir nada → Task 1, `edit_file_requires_unique_match_and_valid_utf8`.
- Tool calls que llegan fragmentados en varios SSE deltas, con `id`/`name` partidos, deben reconstruirse completos → Task 2, `aggregates_split_tool_call_fragments`.
- El recorte a 20 mensajes no debe separar un `assistant` con `tool_calls` de sus mensajes `tool` → Task 2, `build_messages_keeps_tool_call_pairs`.
- Si el usuario rechaza una llamada con otras tres detrás, las tres restantes no se ejecutan y el modelo recibe el error → Task 3, `denial_stops_remaining_calls`.
- Cerrar el chat durante una ronda cancela el bucle y descarta el permiso pendiente, sin dejar diálogos huérfanos → Task 3, `cancel_turn_clears_permission`.
- Sin pestaña activa o sin cwd, la herramienta falla con mensaje claro y no abre diálogo → Task 3, `new_tool_turn_requires_active_cwd`.

---

### Task 1: Módulo de herramientas y política de rutas

**Files:**
- Create: `src/tools.rs` (con tests inline)
- Modify: `src/main.rs` (añadir `mod tools;`)

**Interfaces:**
- Consumes: `gitinfo::expand_home(text: &str, home: Option<&str>) -> Option<String>`.
- Produces:
  - `pub enum ToolName { ListDir, ReadFile, WriteFile, EditFile }` con `as_str() -> &'static str` y `from_str(&str) -> Option<Self>`.
  - `pub struct ToolCall { pub id: String, pub name: String, pub arguments: String }`.
  - `pub struct ToolOutcome { pub call_id: String, pub content: String, pub changed_path: Option<String> }`.
  - `pub struct PermissionKey { pub tool: ToolName, pub path: PathBuf }` (deriva `Clone, Debug, PartialEq, Eq, Hash`).
  - `pub fn tool_definitions() -> Vec<serde_json::Value>`.
  - `pub fn resolve_path(raw: &str, cwd: &Path, home: Option<&str>) -> Result<PathBuf, String>`.
  - `pub fn is_within(path: &Path, cwd: &Path) -> bool`.
  - `pub fn execute(call: &ToolCall, cwd: &Path, home: Option<&str>) -> Result<ToolOutcome, String>`.
  - Constantes `pub const MAX_READ_BYTES: usize = 65_536;`, `pub const MAX_WRITE_BYTES: usize = 262_144;`, `pub const MAX_LIST_ENTRIES: usize = 200;`.

- [ ] **Step 1: Escribir los tests que fallan en `src/tools.rs`**

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
        arguments: r#"{"path":"f.txt","old":"one","new":"two","reason":"cambiar"}"#.into() };
    assert!(execute(&call, &root, None).is_err());
    std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
    assert!(execute(&call, &root, None).is_err());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn write_file_rejects_content_over_limit() {
    let root = std::env::temp_dir().join(format!("repterm-write-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let args = format!(r#"{{"path":"big.txt","content":"{}","reason":"prueba"}}"#, "x".repeat(MAX_WRITE_BYTES + 1));
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
        arguments: r#"{"path":"sub/a.txt","content":"hola","reason":"crear"}"#.into() };
    let outcome = execute(&write, &root, None).unwrap();
    assert_eq!(outcome.changed_path.as_deref(), Some("sub/a.txt"));
    let read = ToolCall { id: "r".into(), name: "read_file".into(),
        arguments: r#"{"path":"sub/a.txt","reason":"leer"}"#.into() };
    assert!(execute(&read, &root, None).unwrap().content.contains("hola"));
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

- [ ] **Step 2: Verificar que fallan**

Run: `cargo test tools::tests`
Expected: FAIL porque `src/tools.rs` no existe todavía.

- [ ] **Step 3: Crear el módulo con tipos, constantes y `tool_definitions`**

En `src/tools.rs` declara los tipos y constantes de Interfaces. `tool_definitions()` devuelve cuatro objetos `{"type":"function","function":{"name":…,"description":…,"parameters":{…}}}`. Cada `parameters` es `type:"object"` con `path` y `reason` obligatorios; `write_file` añade `content`, `edit_file` añade `old` y `new`, todos obligatorios. Las descripciones son frases breves en español. No escribas aún el cuerpo de `execute`.

- [ ] **Step 4: Implementar resolución y contención de rutas**

`resolve_path` expande `~` con `expand_home`, une relativas a `cwd`, y canonicaliza; si no existe, canonicaliza el ancestor existente más cercano y reconstruye los componentes restantes. `is_within` compara componentes con `Path::starts_with` sobre rutas canonicalizadas.

- [ ] **Step 5: Implementar `execute`**

Parsea `arguments` como `serde_json::Value` y exige `reason` no vacío. `read_file`: UTF-8 estricto, trunca a `MAX_READ_BYTES` e indica truncamiento. `write_file`: rechaza más de `MAX_WRITE_BYTES`, crea padres y sobrescribe; devuelve `changed_path` relativo a `cwd`. `edit_file`: comprueba metadata antes de leer, exige UTF-8 válido, cuenta apariciones de `old`, exige exactamente una, y devuelve `changed_path`. `list_dir`: un nivel, ordena, marca directorios con `/`, muestra tamaños de archivos y limita a `MAX_LIST_ENTRIES` indicando el recorte. Un nombre desconocido devuelve `Err`.

- [ ] **Step 6: Añadir el módulo al binario**

En `src/main.rs`, añade `mod tools;` junto a los `mod` existentes.

- [ ] **Step 7: Verificar los tests**

Run: `cargo test tools::tests`
Expected: PASS con los seis tests anteriores.

- [ ] **Step 8: Commit**

```bash
git add src/tools.rs src/main.rs
git commit -m "feat: add scoped filesystem tools and path policy"
```

---

### Task 2: Protocolo de tool calls en el cliente LiteLLM

**Files:**
- Modify: `src/llm.rs`

**Interfaces:**
- Consumes: `tools::ToolCall`, `tools::tool_definitions()`.
- Produces:
  - `Role::Tool`.
  - `ChatMessage` con `pub tool_calls: Option<Vec<ToolCall>>` y `pub tool_call_id: Option<String>` (ambos `skip_serializing_if = "Option::is_none"`), más `pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self` y `pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self`.
  - `pub struct ToolCallFragment { pub index: usize, pub id: Option<String>, pub name: Option<String>, pub arguments: Option<String> }`.
  - `pub fn parse_tool_call_delta(frame: &str) -> Result<Vec<ToolCallFragment>, String>`.
  - `pub fn aggregate_tool_calls(fragments: &[ToolCallFragment]) -> Vec<ToolCall>`.
  - `pub fn completion_payload(model: &str, messages: &[ChatMessage], tools: &[serde_json::Value]) -> serde_json::Value`.
  - `StreamUpdate::ToolCalls(Vec<ToolCall>)`.
  - `LlmClient::stream(&self, model: &str, messages: &[ChatMessage], tx: &Sender<StreamUpdate>, tools: &[serde_json::Value]) -> Result<(), String>`.
  - `build_messages(context: &str, history: &[ChatMessage]) -> Vec<ChatMessage>` (firma igual, nuevo agrupamiento).

- [ ] **Step 1: Escribir los tests que fallan**

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
        ChatMessage::user("lee el archivo"),
        ChatMessage::assistant_tool_calls(vec![ToolCall { id: "c1".into(), name: "read_file".into(), arguments: "{}".into() }]),
        ChatMessage::tool_result("c1", "contenido"),
    ];
    let msgs = build_messages("CTX", &history);
    let roles: Vec<_> = msgs.iter().skip(1).map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
}

#[test]
fn completion_payload_includes_tools_when_nonempty() {
    let defs = tool_definitions();
    let payload = completion_payload("m", &[ChatMessage::user("hola")], &defs);
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

- [ ] **Step 2: Verificar que fallan**

Run: `cargo test llm::tests`
Expected: FAIL por símbolos y variantes aún no definidos.

- [ ] **Step 3: Ampliar los tipos del protocolo**

Añade `Role::Tool`, los dos campos opcionales y los constructores a `ChatMessage`, y la variante `StreamUpdate::ToolCalls(Vec<ToolCall>)`. Los constructores existentes dejan los nuevos campos a `None`. No cambies la firma de `stream` todavía.

- [ ] **Step 4: Implementar parser y agregador**

`parse_tool_call_delta` lee `/choices/0/delta/tool_calls`, extrae `index` y los campos opcionales `id`, `type`, `function.name` y `function.arguments`; ignora frames sin tool calls. `aggregate_tool_calls` agrupa por `index` en orden de aparición, conserva el primer `id`/`name` no vacío y concatena los fragmentos de `arguments`.

- [ ] **Step 5: Implementar `completion_payload` y usarlo en `stream`**

Construye el JSON con `model`, `stream: true`, `messages` y, solo si `tools` no está vacío, `tools` y `tool_choice: "auto"`. Cambia la firma de `stream` para recibir `tools` y usa `completion_payload`. En el bucle, acumula los fragmentos devueltos por `parse_tool_call_delta`; al recibir `[DONE]` o EOF, si hay fragmentos envía `StreamUpdate::ToolCalls(aggregate_tool_calls(...))`, si no envía `Done`. Mantén el comportamiento actual de `Delta`, `Error` y cancelación.

- [ ] **Step 6: Reescribir el system prompt**

Sustituye la frase que niega herramientas por una descripción breve de las cuatro operaciones, la regla «solo el directorio de la pestaña; fuera se pide permiso» y la obligación de incluir `reason` en cada llamada.

- [ ] **Step 7: Ajustar `build_messages`**

Agrupa cada `assistant` con `tool_calls` y sus mensajes `tool` posteriores como una unidad indivisible al aplicar `HISTORY_LIMIT = 20`. El mensaje `system` sigue refrescándose en cada petición.

- [ ] **Step 8: Verificar los tests**

Run: `cargo test llm::tests`
Expected: PASS con los cuatro tests anteriores y los tres preexistentes.

- [ ] **Step 9: Commit**

```bash
git add src/llm.rs
git commit -m "feat: support streamed tool calls in LiteLLM client"
```

---

### Task 3: Estado del turno, ejecución y permisos en la app

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: `tools::{ToolCall, ToolOutcome, ToolName, PermissionKey, execute, is_within, tool_definitions}`, `llm::{StreamUpdate, ChatMessage}`.
- Produces:
  - `pub struct PermissionRequest { pub call: ToolCall, pub key: PermissionKey, pub reason: String, pub index: usize, pub total: usize }`.
  - `pub struct ToolTurn { pub tab_id: u64, pub cwd: PathBuf, pub round: usize, pub calls: Vec<ToolCall>, pub outcomes: Vec<ToolOutcome>, pub next: usize, pub permission: Option<PermissionRequest>, pub stopped: bool }`.
  - `pub enum PermissionDecision { AllowOnce, AllowAlways, Deny }`.
  - `pub enum ToolAction { Execute(ToolCall), Ask(PermissionRequest), Finish }`.
  - `ChatState` añade `pub turn: Option<ToolTurn>`.
  - `App` añade `allowed_tools: HashSet<PermissionKey>`.
  - Funciones libres: `fn new_tool_turn(tab_id: Option<u64>, cwd: Option<&str>) -> Result<ToolTurn, String>`, `fn next_tool_action(turn: &ToolTurn, allowed: &HashSet<PermissionKey>) -> ToolAction`, `fn apply_permission(turn: &mut ToolTurn, allowed: &mut HashSet<PermissionKey>, decision: PermissionDecision)`, `fn clear_tool_turn(chat: &mut ChatState)`.
  - Métodos: `fn spawn_chat_request(&mut self, ctx: &egui::Context)`, `fn advance_tools(&mut self, ctx: &egui::Context)`, `fn resolve_permission(&mut self, decision: PermissionDecision, ctx: &egui::Context)`.

- [ ] **Step 1: Escribir los tests que fallan en `src/app.rs`**

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
            arguments: r#"{"path":"/etc/hosts","reason":"diagnóstico"}"#.into() }],
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
        arguments: r#"{"path":"/etc/hosts","reason":" diagnóstico"}"#.into() };
    let mut turn = ToolTurn { tab_id: 1, cwd, round: 1,
        calls: vec![call("a"), call("b"), call("c")], outcomes: vec![], next: 0,
        permission: Some(PermissionRequest { call: call("a"),
            key: PermissionKey { tool: ToolName::ReadFile, path: PathBuf::from("/etc/hosts") },
            reason: "diagnóstico".into(), index: 1, total: 3 }), stopped: false };
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

- [ ] **Step 2: Verificar que fallan**

Run: `cargo test app::tests`
Expected: FAIL por tipos y funciones ausentes.

- [ ] **Step 3: Añadir los tipos y funciones puras**

Define los tipos de Interfaces en `src/app.rs`, añade `turn` a `ChatState` y `allowed_tools` a `App` (inicializado vacío en `new`). Implementa `new_tool_turn`, `next_tool_action` y `apply_permission`. `next_tool_action` devuelve `Ask` cuando la ruta canonicalizada está fuera del cwd y no está en `allowed`; `Execute` en caso contrario; `Finish` cuando `next >= calls.len()` o `stopped`. `AllowAlways` inserta la clave en `allowed`; `Deny` marca `stopped`, no inserta nada y limpia `permission`.

- [ ] **Step 4: Conectar el streaming con el turno**

En `send_chat`, antes de lanzar el hilo, crea el turno con `new_tool_turn(self.tabs.active(), self.active_cwd().as_deref())`; si falla, muestra el error y no envía. Guarda el turno en `chat.turn`. `spawn_chat_request` construye los mensajes con `build_messages(&self.chat_context(), &self.chat.messages)` y llama a `client.stream(..., &tool_definitions())` en el hilo, reutilizando el `cancel` actual.

- [ ] **Step 5: Manejar `StreamUpdate::ToolCalls` en `poll_chat`**

Al recibir tool calls, guárdalos en `turn.calls`, conserva `partial` como texto del asistente y no cierres el turno. Al recibir `Done`: si `turn.calls` está vacío, cierra el turno como hoy; si no, añade el mensaje `assistant_tool_calls`, reinicia `partial`, y llama a `advance_tools`.

- [ ] **Step 6: Implementar `advance_tools` y `resolve_permission`**

`advance_tools` itera con `next_tool_action`: `Execute` llama a `execute`, guarda `ToolOutcome` y avanza; `Ask` guarda el `PermissionRequest` y espera; `Finish` añade un `ChatMessage::tool_result` por cada outcome (o un error explícito si hubo denegación), añade la nota «Archivo modificado: …» para los `changed_path`, y decide: si `stopped` o `round >= 8`, cierra el turno con un mensaje de aviso cuando corresponda; si no, incrementa `round` y vuelve a `spawn_chat_request`. `resolve_permission` aplica la decisión, limpia `permission` y reanuda `advance_tools`.

- [ ] **Step 7: Cancelación y cierre**

Implementa `clear_tool_turn` para dejar `chat.turn = None` sin tocar `allowed_tools`. Amplía `cancel_chat` para que limpie el turno **antes** del `let Some(rx) = ... else { return; }` actual, ponga `cancelled` y cancele el flag compartido. Haz que `close_chat` llame a `cancel_chat` cuando haya un turno en curso.

- [ ] **Step 8: Verificar los tests y la compilación**

Run: `cargo test app::tests`
Expected: PASS con los cinco tests anteriores y los existentes.

- [ ] **Step 9: Commit**

```bash
git add src/app.rs
git commit -m "feat: run agent file tools with scoped permissions"
```

---

### Task 4: Diálogo de permiso y resultados en el chat

**Files:**
- Modify: `src/app.rs`
- Modify: `src/chat_view.rs`

**Interfaces:**
- Consumes: `PermissionRequest`, `PermissionDecision`, `ToolOutcome` de Tasks 1-3.
- Produces: `pub fn tool_result_summary(content: &str) -> String` en `src/chat_view.rs`; `fn permission_rows(request: &PermissionRequest) -> [(&'static str, String); 4]` en `src/app.rs`; método `fn show_permission(&mut self, ctx: &egui::Context)`.

- [ ] **Step 1: Escribir los tests que fallan**

En `src/chat_view.rs`:

```rust
#[test]
fn tool_result_summary_is_one_line() {
    let summary = tool_result_summary("list_dir: 42 entradas\na.txt\nb.txt");
    assert_eq!(summary, "list_dir: 42 entradas");
}
```

En `src/app.rs`:

```rust
#[test]
fn permission_rows_show_tool_path_cwd_and_reason() {
    let req = PermissionRequest { call: ToolCall { id: "c1".into(), name: "read_file".into(), arguments: "{}".into() },
        key: PermissionKey { tool: ToolName::ReadFile, path: PathBuf::from("/etc/hosts") },
        reason: "diagnóstico".into(), index: 1, total: 2 };
    let rows = permission_rows(&req);
    assert_eq!(rows[0].0, "Herramienta");
    assert_eq!(rows[0].1, "read_file");
    assert_eq!(rows[1].1, "/etc/hosts");
    assert_eq!(rows[3].1, "diagnóstico");
}
```

- [ ] **Step 2: Verificar que fallan**

Run: `cargo test tool_result_summary_is_one_line permission_rows_show_tool_path_cwd_and_reason`
Expected: FAIL por funciones ausentes.

- [ ] **Step 3: Implementar el resumen de resultados**

`tool_result_summary` devuelve la primera línea del contenido, recortada a 100 caracteres, o `resultado de herramienta` si está vacío. En `show_chat`, los mensajes `Role::Tool` se dibujan con ese resumen en monospace y un botón «Ver» que expande el contenido completo; no se les añaden bloques «Insertar en terminal».

- [ ] **Step 4: Implementar `permission_rows` y el diálogo**

`permission_rows` devuelve las cuatro filas `Herramienta`, `Ruta`, `Directorio de la pestaña` y `Motivo`, con la ruta y el cwd en texto plano. `show_permission` dibuja un `egui::Window` no redimensionable de 420×220 con esas filas, un título `Permiso solicitado (1 de N)` y tres botones: «Permitir una vez», «Permitir siempre esta ruta» y «Rechazar». `Escape` y el cierre de la ventana se resuelven como `Deny`.

- [ ] **Step 5: Conectar precedencia y estado visual**

En `update`, cuando haya un permiso pendiente, consume `Escape` antes que cualquier otro atajo y dibuja `show_permission` antes que `show_chat`. En la cabecera del chat, muestra un punto ámbar con `ronda N/8` mientras `chat.turn.is_some()`. Desactiva el compositor y el botón «Enviar» mientras haya turno en curso, no solo mientras haya permiso.

- [ ] **Step 6: Verificar los tests**

Run: `cargo test`
Expected: PASS con los dos tests nuevos y toda la suite previa.

- [ ] **Step 7: Commit**

```bash
git add src/app.rs src/chat_view.rs
git commit -m "feat: add permission dialog and tool result summaries"
```

---

### Task 5: Verificación final y cierre

**Files:**
- Modify: solo si aparece algún fallo de verificación: `src/tools.rs`, `src/llm.rs`, `src/app.rs`, `src/chat_view.rs`.

**Interfaces:**
- Consumes: todas las anteriores.

- [ ] **Step 1: Formato**

Run: `cargo fmt --all`
Expected: termina sin errores.

- [ ] **Step 2: Comprobar formato**

Run: `cargo fmt --all -- --check`
Expected: sin salida.

- [ ] **Step 3: Suite completa**

Run: `cargo test`
Expected: todos los tests pasan, incluidos los nuevos de tools, llm, app y chat_view.

- [ ] **Step 4: Lint estricto**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: sin avisos.

- [ ] **Step 5: Comprobar diff y estado**

Run: `git status --short && git diff --stat a969321..HEAD`
Expected: `README.md` sigue apareciendo como modificado pero **no** incluido en los commits del plan; el resto de archivos cambiados son los de este plan.

- [ ] **Step 6: Revisión manual del diff completo**

Lee `git diff a969321..HEAD` buscando: rutas que puedan escapar del cwd sin permiso, tool calls que se pierdan al recortar historial, permisos que se persistenten por error, y mensajes de error que puedan incluir la clave de API. Anota cualquier hallazgo y corrígelo en un único commit si aparece.

- [ ] **Step 7: Commit de correcciones (solo si hay cambios)**

```bash
git add src/tools.rs src/llm.rs src/app.rs src/chat_view.rs
git commit -m "fix: address final review findings"
```
