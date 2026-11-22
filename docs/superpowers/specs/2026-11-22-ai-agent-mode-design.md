# Modo agente IA en repterm — diseño

## Intención y por qué

El usuario quiere alternar entre el terminal y una conversación con un agente de IA sin salir de repterm, usando `Ctrl+Shift+Enter` o un prefijo `!`. El agente sirve para conversational sobre el estado real del repo (directorio, rama, cambios pendientes y salida del terminal) y para proponer comandos. El proveedor será el proxy LiteLLM que el usuario ya tiene corriendo en Docker, con interfaz de ajustes dentro de la propia app.

Esta especificación cubre la primera versión: **conversar y proponer**. No incluye herramientas ni ejecución automática.

## Objetivos

- Alternar entre modo terminal y modo chat con `Ctrl+Shift+Enter` en cualquier momento, conservando el PTY y el historial tal cual.
- Alternar escribiendo `!`: en el terminal abre el chat; en el chat devuelve al terminal, y `!comando` escribe ese comando en el PTY activo y pulsa Enter (lo ejecuta). El botón «Insertar en terminal» de los bloques de código, en cambio, inserta sin ejecutar.
- Conversar con un modelo servido por LiteLLM mediante la API compatible con OpenAI, con respuesta en streaming.
- Configurar URL base, clave y modelo desde la interfaz de repterm, guardándolos localmente.
- Enviar como contexto el directorio activo, la rama, la lista completa de archivos pendientes de `RepoStatus.changes` (ruta, estado staged, líneas +/−), la selección del terminal si existe y las últimas líneas visibles.
- Mostrar los comandos propuestos como bloques de código con una acción para insertarlos en el terminal (sin ejecutarlos).

## No objetivos

- Tool calls, lectura/escritura de archivos por parte del modelo, ni ejecución automática de comandos.
- Soporte para varios proveedores además de LiteLLM en esta versión.
- Creación de ramas, stage/discard/commit o cualquier otra acción de git (la integración actual solo lista y muestra diffs).
- Imágenes en markdown, resaltado de sintaxis externo pesado, ni sincronización de la conversación con sesiones de opencode/claude.

## Restricciones

- Rust 2021, edición y toolchain actuales (rustc 1.99.1 disponible).
- eframe/egui 0.31; `egui_term` vendorizado en `vendor/egui_term`. Evitar cambios en vendor si es posible.
- El sondeo de git ya usa `std::thread` + `mpsc`; el cliente HTTP debe seguir ese patrón (sin añadir tokio).
- El foco lo gestiona `TerminalView::set_focus(...)` en `src/app.rs:739` y `egui_term` pide foco cada frame cuando tiene `has_focus`; cualquier TextEdit nuevo debe protegerse de ese robo de foco, igual que se hizo con el picker de ramas.
- Atajos existentes en `src/app.rs:665-687` (`Ctrl+T/W/Delete/PageUp/PageDown`) deben seguir funcionando y tener prioridad explícita sobre los nuevos.

## Decisiones tomadas

1. **Proveedor.** LiteLLM proxy, API compatible con OpenAI: `POST {base_url}/chat/completions` con `stream: true`, deltas en `choices[0].delta.content`, fin en `data: [DONE]`. Descubrimiento de modelos con `GET {base_url}/models` (forma `{ "data": [ { "id": "..." } ] }`).
2. **Cliente HTTP.** `ureq = { version = "3.5", features = ["json"] }` (blocking, rustls por defecto, MSRV 1.85 < 1.99). Leer el cuerpo como `Read` desde un hilo, juntar buffers y parsear tramas SSE separadas por `\n\n`. Un `AtomicBool` permite cancelar el streaming desde la UI.
3. **Ajustes.** Archivo `$XDG_CONFIG_HOME/repterm/settings.json` (o `~/.config/repterm/settings.json`), serializado con `serde`/`serde_json` (ya se añaden para la API). Campos: `base_url` (por defecto `http://localhost:4000/v1`), `api_key`, `model`, `include_terminal_context: bool`. Permisos `0600` al crear el archivo en Unix. La clave no se escribe en logs ni aparece en la conversación; el campo de ajustes se precarga enmascarado desde disco y puede revelarse con el botón de ojo.
4. **Interacción `!`.** Se intercepta solo cuando la línea actual del terminal está vacía (ningún carácter escrito desde el último Enter), para no romper la expansión de historial de bash. Mecanismo verificado: `egui_term` clona los eventos de `ctx` en `vendor/egui_term/src/view.rs:152-154`, así que `App::update` puede retirar el `Event::Text("!")` de `ctx.input_mut().events` antes de dibujar los paneles, sin tocar vendor. Al detectarlo se cambia al chat y se mueve el foco al compositor; el resto de lo que escriba el usuario va al chat. En el chat, `!` al inicio del compositor vuelve al terminal; si hay texto detrás, se escribe en el PTY activo y se pulsa Enter.
5. **Contexto y petición.** Se construye en la UI antes de lanzar la petición: cwd de la pestaña activa, `format_git_label` + lista de `RepoStatus.changes`, selección de `backend.selectable_content()` si existe, y como máximo las últimas 120 líneas visibles del grid. Ese contexto se envía como mensaje `system` **en cada turno** (refrescado), seguido del historial; se reenvían como máximo los últimos 20 mensajes para controlar coste y límites. `include_terminal_context = false` omite tanto las 120 líneas como la selección; cwd, rama y archivos pendientes se envían siempre. Límite duro de caracteres para el bloque de terminal.
6. **Respuestas.** Markdown sencillo (texto, listas, código) con `pulldown-cmark` 0.13 dibujado mediante `LayoutJob`, para no acoplar la versión de egui a `egui_commonmark`. Los bloques de código llevan un botón «Insertar en terminal» que escribe el texto en el PTY activo sin Enter.
7. **Estado del modelo.** Selector en ajustes alimentado por `/v1/models` de forma asíncrona, con entrada manual como fallback si el endpoint falla o requiere permisos distintos.
8. **Ámbito de la conversación.** Una sola conversación global en memoria, compartida por todas las pestañas; no se persiste al reiniciar. Los comandos propuestos y los `!cmd` van siempre a la pestaña activa en el momento de usarlos; si no hay pestaña activa se muestra un aviso y no se escribe nada.
9. **System prompt.** El constructor redacta un prompt breve que fija el papel («asistente dentro de un terminal»), exige bloques de código vallados para comandos, prohíbe afirmar que tiene herramientas y pide respuestas concisas. No se expone como ajuste.
10. **Cancelación y streaming.** El botón de envío se desactiva mientras hay un streaming en curso (no se encolan mensajes). Cancelar conserva el texto parcial como mensaje del asistente marcado como «cancelado» y reactiva el compositor; el hilo se detiene mediante el `AtomicBool`.

## Interfaz (brief del diseñador)

- Tres superficies mutuamente excluyentes: terminal (`CentralPanel`), chat (`CentralPanel`, sustituye al `TerminalView`) y ajustes (`Window` modal de ~460×340). La barra de pestañas y los chips de git siguen visibles siempre.
- Ajustes: modal con Base URL, API key enmascarada con opción de revelar y `ComboBox` de modelo alimentada por `/v1/models` con entrada manual como fallback; botones Guardar/Cancelar; `Escape` cierra sin guardar; estados `loading_models`, `saving`, `saved` y `save_error`. Abrir ajustes cierra el picker de ramas; el modal captura todo el teclado.
- Chat: cabecera de ~34 px con «AI chat», punto de estado (verde conectado / ámbar streaming / rojo error), modelo de solo lectura y botón ×; lista de mensajes con burbujas de usuario (violeta `(116,93,215)`) y asistente (`weak_bg_fill`), markdown simple, cursor de bloque parpadeante durante el streaming y ecos `!cmd` como chip monoespacio; compositor monoespacio de 13 px (44–160 px de alto), `Enter` envía y `Shift+Enter` inserta salto.
- Precedencia de teclado: 1) modal de ajustes; 2) `Ctrl+Shift+Enter`; 3) atajos de pestañas; 4) `!` en terminal con línea limpia; 5) eventos PTY; 6) `Enter` en el compositor (`!cmd` vuelve al terminal y ejecuta); 7) envío al modelo.
- En modo chat el `TerminalView` no se añade al árbol de widgets, así que no puede robar el foco del compositor. Refuerzo: `.set_focus(self.picker.is_none() && !chat_mode)`.
- Espaciado y paleta: `item_spacing` 4 (pestañas), 6 (chips de git) y 8 (cabecera de chat); monoespacio 12 en mensajes; burbujas con padding `10×12`, `corner_radius(10)` y ancho máximo del 70 % del área central; ADD `(95,184,106)`, DEL `(224,108,108)`, WARN `(217,164,65)` y CHAT_ACCENT `(116,93,215)`.

## Lo que queda al constructor

- La distribución exacta de widgetos, espaciado, colores, estados de carga/error y precedencia de teclado la fija el diseñador en su propio brief; esta spec solo fija el comportamiento.
- Implementación en dos carriles independientes: backend de API/SSE + ajustes (`src/llm.rs`, `src/settings.rs`) y condensación del modo chat en `src/app.rs`. No solapar scopes de escritura.
- La detección de «línea limpia» se implementa con un flag por pestaña (true al crear la pestaña y tras cada Enter, false al escribir cualquier otro texto), no inspeccionando el grid; el `!` se retira del stream de eventos antes de que `TerminalView` lo vea.
- Pruebas unitarias: parser SSE (deltas múltiples, `[DONE]`, evento de error), carga/guardado de ajustes, construcción de contexto con/sin selección, toggle de modo y detección de prefijo.

## Riesgos conocidos

- La clave se guarda en texto plano en el archivo de ajustes (`0600`); es la contrapartida de la opción elegida de «ajustes en la app».
- La salida del terminal puede contener secretos; por eso existe `include_terminal_context`.
- Un `!` interceptado en el momento equivocado puede impedir comandos legítimos de bash con historial; la regla «solo con línea vacía» limita el daño.
