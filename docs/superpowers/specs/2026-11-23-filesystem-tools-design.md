# Herramientas de archivos para el agente IA en repterm — diseño

## Intención y por qué

El usuario quiere que el agente de repterm deje de limitarse a proponer comandos y pueda trabajar directamente sobre el proyecto abierto: leer archivos, crear otros nuevos y modificar los existentes. La condición que puso es un límite de ámbito, no un permiso general: «solo quiero q pueda acceder al directorio actual, para fuera debe pedir permiso explicitamente».

Cuando se le preguntó cómo debía ser ese permiso, eligió un diálogo detallado con botones: mostrar la herramienta, la ruta, el directorio de la pestaña y el motivo, y ofrecer permitir una vez, permitir siempre esa ruta o rechazar. Aceptó también los supuestos confirmados en la conversación: dentro del directorio no hay confirmación; «permitir siempre» vale solo para la sesión actual; al rechazar, el modelo recibe un error claro y puede seguir conversando.

Esta spec amplía el modo agente ya implementado en `docs/superpowers/specs/2026-11-22-ai-agent-mode-design.md`. Mantiene su regla de que el modelo **no ejecuta comandos por su cuenta**.

## Objetivos

- Dar al modelo cuatro herramientas de archivos: `list_dir`, `read_file`, `write_file` y `edit_file`.
- Resolver todas las rutas relativas al cwd de la pestaña activa; permitir libremente las que queden dentro de ese directorio.
- Pedir permiso explícito mediante un diálogo modal detallado para cualquier operación cuya ruta canonicalizada quede fuera del cwd, incluidos los enlaces simbólicos que apunten fuera.
- Soportar tool calls en streaming con la API compatible con OpenAI de LiteLLM, encadenando varias rondas de herramientas hasta que el modelo dé una respuesta final.
- Recordar durante la sesión las rutas autorizadas con «permitir siempre», sin escribir esa lista en disco.
- Informar en el chat de los archivos creados o modificados, para que el usuario pueda revisarlos con el panel de cambios ya existente.

## No objetivos

- Ejecución automática de comandos de shell; `!comando` sigue siendo una acción iniciada por el usuario en el PTY.
- Borrar, renombrar, mover o cambiar permisos de archivos; operaciones de git (stage, commit, checkout); acceso a red más allá del propio LiteLLM.
- Persistir los permisos «siempre» entre reinicios de repterm, ni ofrecer una interfaz para revocar la lista de la sesión: se reinicia al cerrar la app.
- Aislamiento a nivel de sistema operativo (contenedores, sandbox, seccomp): el límite es una comprobación de rutas en la aplicación.
- Vista previa de diffs dentro del diálogo de permiso; se deja para una versión posterior.
- Varios diálogos de permiso simultáneos: se encolan y se muestran de uno en uno.
- Incluir en este cambio el `README.md` con la sección de Oh My Zsh que el usuario dejó modificado pero sin commitear.

## Restricciones

- Rust 2021, rustc/cargo 1.99.1, eframe/egui 0.31, `egui_term` vendorizado: no tocar `vendor/egui_term`.
- Sin tokio: el cliente HTTP ya usa `std::thread` + `mpsc` en `src/llm.rs:127-223`; la espera a un permiso debe ser estado en la UI, no un bloqueo del hilo.
- Seguir los patrones existentes: ventanas modales de `src/app.rs` (`show_settings`, `show_picker`), `active_cwd()` en `src/app.rs:883-887`, `expand_home()` en `src/gitinfo.rs:48-60` y el formato de guardado de `src/settings.rs`.
- La clave de API no debe aparecer nunca en logs ni en la conversación; los mensajes de herramienta no la incluyen.
- El árbol de trabajo tiene un cambio ajeno sin commitear (`README.md`, Oh My Zsh); cualquier commit de este trabajo debe añadir solo sus propios archivos.

## Decisiones tomadas

1. **Protocolo de herramientas.** Peticiones OpenAI-compatible con `tools: [{type:"function", function:{name,description,parameters}}]` y `tool_choice: "auto"`. Los mensajes `assistant` pueden llevar `tool_calls: [{id,type:"function",function:{name,arguments}}]`; los resultados se devuelven como `role:"tool", tool_call_id, content`. Todas las definiciones y el parsing de fragmentos viven junto al cliente en `src/llm.rs` salvo que el constructor decida extraer un módulo propio.

2. **Herramientas y sus argumentos.** `list_dir(path)`, `read_file(path)`, `write_file(path, content)` y `edit_file(path, old, new)`. Todas exigen además `reason: string`, una frase breve que se muestra tal cual en el diálogo. No existe una herramienta de borrado. `edit_file` exige que `old` aparezca una sola vez; si no aparece o aparece varias, devuelve un error al modelo sin escribir nada.

3. **Resolución de rutas.** `~` se expande con `expand_home`. Las rutas relativas se resuelven contra el cwd de la pestaña activa. Se usa `canonicalize` siempre que sea posible; para archivos que aún no existen se canonicaliza el ancestor existente más cercano y se reconstruye el resto. La ruta resultante debe empezar por el cwd canonicalizado. Un `..`, una ruta absoluta o un symlink que salga del directorio caen fuera y disparan el permiso. Sin pestaña activa, la herramienta devuelve un error y no abre diálogo, igual que `command_target(None)`.

**Decisión confirmada por el usuario:** el cwd y la pestaña se fijan al iniciar el turno. Todas las llamadas de todas las rondas de ese turno usan el mismo cwd canonicalizado y el mismo `tab_id`, aunque el usuario haga `cd` o cambie de pestaña mientras tanto. El diálogo muestra siempre ese cwd fijado. Así el permiso «dentro del directorio» no puede ampliarse a mitad de petición.

4. **Política de permisos.** Dentro del cwd: ejecución directa, sin diálogo. Fuera: se encola una petición con operación, ruta canonicalizada, cwd y motivo. El diálogo ofrece «Permitir una vez», «Permitir siempre esta ruta» (clave session-only por pareja operación+ruta canonicalizada) y «Rechazar». `Escape` y cerrar la ventana equivalen a rechazar. El rechazo se devuelve al modelo como contenido de herramienta indicando que el usuario lo denegó; el modelo puede continuar.

5. **Bucle de herramientas.** Al terminar un streaming con `finish_reason = "tool_calls"`, no se cierra el turno: se ejecutan las herramientas en orden (pidiendo permiso cuando toque), se añaden el mensaje `assistant` con esas tool calls y los mensajes `tool` con sus resultados, y se lanza una nueva petición con el mismo contexto refrescado. Límite de 8 rondas por turno, decisión nuestra, para evitar bucles descontrolados; al alcanzarse se informa al usuario. Cancelar durante cualquier ronda detiene el bucle y conserva lo ya escrito, igual que la cancelación actual del streaming.

6. **Streaming y parser.** Los deltas `choices[0].delta.tool_calls` se agrupan por `index`: `id` y `function.name` suelen llegar en el primer fragmento y `function.arguments` se concatena. `finish_reason: "tool_calls"` llega antes de `data: [DONE]`. Se reutiliza `parse_sse_frames`; `parse_delta` se amplía sin romper los tests actuales de contenido. El recorte a 20 mensajes de `build_messages` no debe separar una tool call de su resultado.

7. **Diálogo (opción C elegida).** `Window` modal no redimensionable de unas 420×220 px, con filas `Herramienta`, `Ruta`, `Directorio de la pestaña` y `Motivo`, y tres botones horizontales. Si hay varias peticiones encoladas se muestra `1 de N`. El color de acento violeta y el estilo de botones son los ya usados en ajustes. Precedencia de teclado: permiso > ajustes > chat > pestañas > terminal. Mientras haya un permiso pendiente, el compositor y el botón de envío quedan desactivados; la cabecera del chat muestra un indicador ámbar con el número de rondas en curso.

8. **Escritura y límites.** `write_file` crea los directorios padre que falten y sobrescribe el archivo; cuando el destino está fuera del cwd, el diálogo lo indica explícitamente. Para evitar bloqueos de la UI, `read_file` limita la salida a 64 KiB e informa de truncamiento, y `write_file` rechaza contenidos de más de 256 KiB con un error comprensible para el modelo. Estos tres números son decisión nuestra, no del usuario.

9. **Trazabilidad.** Después de cada `write_file` o `edit_file` con éxito se añade al chat una nota breve del tipo «Archivo modificado: `src/foo.rs`». No se muestra un diff completo: para eso está el panel de cambios existente. Las tool calls y sus resultados cuentan para el límite de 20 mensajes del historial.

10. **System prompt.** Se actualiza el prompt actual, que hoy prohíbe afirmar que hay herramientas, para describir las cuatro operaciones disponibles, la regla de ámbito y la obligación de incluir `reason`. No se expone como ajuste.

11. **Resultados en el chat.** El contenido completo viaja al modelo en el mensaje `tool`, pero en la UI se muestra un resumen de una línea (`list_dir: 42 entradas`, `read_file: src/foo.rs, 3.1 KiB`) con un control para expandir el texto completo. Los mensajes de herramienta no se dibujan con los botones «Insertar en terminal» de los bloques de código. Decisión nuestra.

12. **`list_dir`.** Un solo nivel, no recursivo; máximo 200 entradas ordenadas, con `/` para directorios y el tamaño para archivos; si hay más se indica el recorte. Decisión nuestra.

13. **Archivos binarios.** `read_file` y `edit_file` exigen UTF-8 válido: si no lo es, devuelven un error claro al modelo, sin `from_utf8_lossy`. `edit_file` aplica el mismo límite de 256 KiB que `write_file`, comprobado por metadata antes de leer. Decisión nuestra.

14. **Denegación parcial.** Si una llamada se rechaza, se detiene el resto de esa ronda: se devuelven los resultados de lo ya ejecutado y un error explícito para la llamada denegada; las siguientes no se ejecutan. Cada diálogo responde solo a su propia llamada. Decisión nuestra.

15. **Cierre del chat.** Pulsar × en la cabecera durante una ronda cancela el bucle y cualquier permiso pendiente, con efecto equivalente a rechazar. No se muestra un diálogo de permiso con el chat oculto, y no se aceptan mensajes nuevos mientras haya una ronda en curso. Decisión nuestra.

## Interfaz (brief del diseñador)

- El diálogo de permiso es una superficie nueva, distinta del modal de ajustes y del picker de ramas; solo aparece cuando el modelo pide una operación fuera del cwd.
- Cuerpo con cuatro filas de etiqueta/valor en monospace para ruta y cwd; el motivo en texto normal. Botones alineados a la derecha, con «Rechazar» como acción segura por defecto al pulsar `Escape`.
- La conversación sigue siendo la misma vista de chat; los resultados de herramientas se muestran como mensajes discretos, no como burbujas del usuario, y el estado ámbar desaparece al terminar el bucle.

## Lo que queda al constructor

- Distribución exacta de widgets, espaciados y texto de los botones la fija el brief de UI; esta spec solo fija el comportamiento.
- Dónde viven las definiciones JSON de las herramientas y si conviene extraer `src/tools.rs` para no engordar `src/llm.rs` o `src/app.rs`; cualquiera de las dos formas vale mientras no se solapen scopes de escritura.
- Cómo se estructura internamente el estado de la máquina de estados del bucle de herramientas, siempre que la UI nunca bloquee esperando al usuario y `cancel_chat` lo limpie todo.
- Pruebas unitarias obligatorias: fijación de cwd/tab al iniciar el turno, contención de rutas (incluye `..`, ruta absoluta y symlink que escapa), decisión de permiso y lista session-only, agregación de tool calls fragmentadas por SSE, transiciones del bucle (ejecutar → permiso → resultado → nueva petición), rechazo/cancelación (incluida la denegación a mitad de ronda), límites de tamaño, UTF-8 inválido y ausencia de pestaña activa.
- La comprobación en vivo contra el proxy LiteLLM del usuario no es ejecutable en el entorno de desarrollo; se documentará como limitación.

## Riesgos conocidos

- El modelo puede leer secretos dentro del cwd (por ejemplo `.env`) y reproducirlos en el chat. `include_terminal_context = false` no cubre este caso; el límite de confianza es el propio directorio que el usuario elige como cwd.
- «Permitir siempre» solo recuerda la sesión: al reiniciar vuelven las preguntas. Es la contrapartida de no persistir autorizaciones en disco.
- La plataforma LiteLLM puede variar en cómo propaga los deltas de tool calls; el parser debe tratar fragmentos parciales y no asumir que nombre e identificador llegan completos en el primer chunk.
- Un modelo insistente podría encadenar muchas rondas; el límite de 8 y el botón de cancelación contienen el riesgo.
- Leer o escribir archivos en el hilo de la UI puede producir micro-congelamientos; los límites de tamaño los reducen, pero una README muy grande seguirá siendo costosa.
