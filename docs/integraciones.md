# Integraciones con la suite y servidor MCP (F4.1)

Dos crates de capa L4, sin egui, que la interfaz usará más adelante:

| Crate | Ruta | Para qué |
|---|---|---|
| `iuredesk-integrations` | `crates/integrations` | Hablar con las apps hermanas (IureEditor, IureTranscribe, IureVoz, IureOCR, IureDav, IureProject) |
| `iuredesk-mcp` | `crates/mcp-server` | `iuredesk --mcp`: el PIM local como servidor MCP, puente IPC con la app abierta y registro en Claude/Copilot |

Reglas comunes:

- **Todo bloquea** (procesos hijos, sockets, disco). La UI llama siempre desde el runtime de
  fondo (`Session::spawn_blocking` o equivalente) y lee el resultado en el siguiente cuadro.
- Una app que falta **no es un error**: las acciones devuelven `Outcome::NotInstalled(InstallHint)`
  con un texto es/en (`hint.text.get(spanish)`) y `hint.download_url` para un enlace discreto.
  `Outcome::NotAvailable(Text)` = instalada pero no utilizable ahora (IureVoz cerrado, sin unidad
  montada…). `Err(IntegrationError)` queda para fallos reales; `e.human(spanish)` da el texto.

## 1. `iuredesk-integrations`

### Detección

```rust
use iuredesk_integrations::{Suite, SisterApp, Outcome};

let suite = Suite::detect();               // sin red; unos stat del disco. Guardarla en la sesión.
suite.installed(SisterApp::Editor);        // ¿mostrar «Abrir en IureEditor»?
for a in suite.apps() {                    // panel «Apps de Iurefficient»
    // a.app.name(), a.app.description().get(es), a.path, a.hint (Some si falta)
}
suite.launch(SisterApp::Voz)?;             // botón «Abrir»
```

`Suite::detect()` usa `apps::locate` de connect (Editor, Transcribe, Dav, OCR) y una búsqueda
propia para IureVoz e IureProject: `PATH`, `/usr/bin`, `/usr/local/bin`, `~/.local/bin`,
`/opt/<Producto>` (Linux); `/Applications`, `~/Applications` (macOS, se usa el binario de
`Contents/MacOS` para `--mcp`); `%LOCALAPPDATA%\Programs`, `%ProgramFiles%` (Windows).
Para pruebas: `Suite::empty().with_app(..).with_opener(..).with_voz_socket(..).with_mounts(..)`.

### IureEditor (deep links por el SO)

```rust
use iuredesk_integrations::EditorTarget;

suite.open_in_iureditor(&EditorTarget::File(path))?;                     // iureditor://open?path=
suite.open_in_iureditor(&EditorTarget::Insert { text, path: None })?;   // iureditor://insert?text= (≤ 16 K)
let o = suite.open_in_iureditor(&EditorTarget::NewDocument {            // «Redactar en IureEditor»
    file_name: "Respuesta a Ana".into(), markdown,
})?;                                                                     // o.ready().path = archivo a releer
suite.open_in_iureditor(&EditorTarget::IurefficientDoc {                 // iureditor://iurefficient/doc?case=&doc=&name=&title=
    case_id, doc_id, file_name, case_title,
})?;
```

El enlace se abre con el crate `open`; si el esquema no está registrado se lanza el ejecutable
con el enlace como argumento (IureEditor lo acepta). Para «volver» del correo largo, la UI vigila
`EditorOpened::path` (fecha de modificación) y ofrece «Traer el texto de IureEditor».

### IureTranscribe (cliente MCP)

```rust
let Outcome::Ready(mut job) = suite.transcribe_recording(&audio)? else { /* hint */ };
loop {
    match job.poll(Duration::from_secs(20))? {         // espera hasta 20 s por llamada (máx. 110)
        JobState::Running { percent, stage } => { /* barra de progreso */ }
        JobState::Done(t) => { /* t.text, t.next_offset, t.output("md"), t.language */ break; }
    }
}
let more = job.read_more(offset)?;                    // transcripciones largas
suite.read_transcript(&minuta_md, 0)?;                // archivo ya escrito (.txt/.srt/.vtt/.json/.md)
```

`TranscriptionJob` es dueño del proceso `iuretranscribe --mcp` (los `jobId` viven en él):
soltarlo cancela. Flujo previsto (PLAN §7): cita → «Grabar reunión» → transcripción → minuta →
borrador a los asistentes + tareas.

### IureOCR

```rust
let file = suite.stage_file(&attachment.filename, &bytes)?;      // copia en la caché, nombre saneado
let r = suite.ocr_attachment(&file, &OcrOptions::default())?;     // hasta 15 min
// r.ready() → OcrResult { pdf_path, txt_path, text, skipped, pages, .. }
```

### IureVoz

```rust
suite.voice_ready();                         // ¿hay un IureVoz abierto? (para mostrar el micrófono)
suite.speak("Tienes tres correos nuevos")?;  // Outcome<()>
suite.dictate(Some("Dicta la respuesta"))?;  // Outcome<String> (vacío = no dijo nada)
suite.ask_yes_no("¿Envío el correo?")?;      // Outcome<Option<bool>> (None = sin respuesta)
```

Socket `$XDG_RUNTIME_DIR/iurevoz.sock` primero (Unix); si nadie escucha, `iurevoz --mcp`
(también en Windows). IureVoz cerrado → `NotAvailable("Abre IureVoz…")`.

### IureDav

```rust
suite.dav_mounts();                                   // perfiles de IureDav (connect::apps::iuredav_mounts)
suite.iurefficient_folder();                          // Outcome<DavMount>: montada y con escritura
suite.attachment_destination("contrato.pdf");         // Outcome<PathBuf> (sin escribir; «(2)» si existe)
suite.save_attachment("contrato.pdf", &bytes)?;       // «Guardar adjunto en Iurefficient»
match suite.large_attachment_advice(total_bytes) {    // en el redactor, al adjuntar (≥ 20 MB)
    LinkAdvice::Attach => {}
    LinkAdvice::UseIureDav { mount, text } => { /* barra: «¿Guardar en … y enviar enlace?» */ }
    LinkAdvice::MountIureDav(text) => {}
    LinkAdvice::InstallIureDav(hint) => {}
}
```

### IureProject

`suite.open_in_iureproject(&archivo_iurp)?` (lanza `iureproject <archivo>`; no tiene deep links).
Para otras herramientas MCP de cualquier app: `suite.connect_mcp(app)? → McpClient`
(`list_tools`, `call_tool(nombre, args, timeout)`).

### Cliente MCP genérico

`McpClient::spawn(programa, &["--mcp"], &McpOptions)` hace `initialize` (protocolo
`2025-06-18`) + `notifications/initialized`; `list_tools()` sigue `nextCursor`; `call_tool()`
devuelve `ToolResult { text, structured, is_error }` (`into_ok()` convierte `isError` en
`IntegrationError::Tool`). Tiempos por llamada; al agotarse manda `notifications/cancelled`.
Líneas ≤ 32 MB, lo que no es JSON se ignora, peticiones del servidor se rechazan. Al soltarlo se
mata y espera al hijo. Pruebas: binario `iuredesk-fake-mcp` (no se distribuye) hecho con el mismo
`mcp_server` de connect que usan las apps hermanas.

## 2. `iuredesk-mcp`

### Herramientas

| Herramienta | Argumentos | Devuelve |
|---|---|---|
| `buscar_correo` | `query`, `limit` (1–50) | id, thread_id, carpeta, remitente, asunto, fecha, extracto |
| `leer_hilo` | `thread_id` o `message_id`, `max_chars` (≤ 60 000) | **texto plano** (cabeceras + cuerpo), con tope |
| `crear_borrador` | `to`, `cc`, `subject`, `body_markdown`, `reply_to_message_id?`, `account?` | id del borrador en Borradores. **Nunca envía** |
| `agenda` | `desde`, `hasta` (≤ 92 días), `zona` | apariciones (repeticiones expandidas con `occurrences_between`) |
| `crear_evento` | `titulo`, `inicio`, `fin?`, `lugar?`, `descripcion?`, `calendario_id?`, `zona?` | id; local primero vía `pim_sync::ops::save_event` (cola `pim.event.put`) |
| `buscar_contacto` | `q`, `limit` | nombre, correos, teléfonos, organización |
| `crear_nota` | `title`, `markdown` | id |
| `mis_tareas` | `incluir_terminadas?`, `limit` | tareas por vencimiento y prioridad |

Descripciones en inglés y español en la misma cadena. `crear_borrador` construye el MIME con
`iuredesk-compose` (respuesta con `compose::reply`: cabeceras del hilo y original citado), lo
guarda con `draft + seen` en la carpeta Borradores y encola `append_draft` (`{"id": n}`), igual
que `Session::save_draft`.

### Acceso al almacén: puente IPC

redb (y el índice tantivy) admiten **un solo proceso**. Por eso:

1. La app abierta arranca el puente: socket Unix (Linux/macOS) o TCP `127.0.0.1` con puerto
   efímero (Windows), y escribe `bridge-<hash del almacén>.json` (0600) en
   `$XDG_RUNTIME_DIR/iuredesk` (o la caché del usuario) con la dirección y un token de 256 bits.
2. `iuredesk --mcp` busca ese archivo; si responde, cada llamada corre **dentro de la app** sobre
   su `Arc<Store>` (una línea JSON por conexión, token comparado en tiempo constante, tamaños y
   tiempos acotados). Las escrituras llaman a `BridgeOptions::on_change(nombre)`.
3. Si no hay app, el proceso MCP abre el almacén **solo durante cada llamada** y lo suelta (así la
   app puede arrancar después). Si el almacén está bloqueado y no hay puente → mensaje humano
   «IureDesk está abierto pero no aceptó la consulta…».

### Cambio en `apps/iuredesk/src/main.rs` (lo hace el agente de la app; ~10 líneas)

Cargo: `iuredesk-mcp = { workspace = true }` en `apps/iuredesk/Cargo.toml`.

```rust
// 1) Al principio de main(), antes de cualquier salida a stdout:
if args.first().map(String::as_str) == Some("--mcp") {
    let sample = args.iter().any(|a| a == "--sample");
    let dir = data_dir().join(if sample { "sample" } else { "store" });
    return ExitCode::from(iuredesk_mcp::run_stdio(&dir).clamp(0, 255) as u8);
}
// (y aceptar "--mcp" en la validación de opciones y en HELP)

// 2) En open_session(), tras abrir el almacén y antes de pasarlo a la Session:
let store = Arc::new(store);
let bridge = iuredesk_mcp::start_bridge(store.clone(), iuredesk_mcp::BridgeOptions {
    on_change: Some(Arc::new(move |_tool| { /* marcar «recargar» para la UI (p. ej. un AtomicBool) */ })),
    ..Default::default()
}).ok();   // si falla, la app sigue; el MCP dirá que IureDesk está abierto
// guardar `bridge` (BridgeHandle) mientras viva la ventana: al soltarlo se cierra y se borra el punto de acceso
```

`on_change` corre en el hilo del puente: que solo marque una bandera; la UI, en su siguiente
cuadro, recarga carpetas/notas/agenda (`data_gen`). Lo ideal es que `Session` exponga un
`Arc<AtomicBool>` «datos cambiados fuera».

### Botón «Usar IureDesk desde Claude / Copilot» (ajustes)

```rust
let st = iuredesk_mcp::agents_status();             // st.claude_desktop.{installed,connected,stale}, st.vscode.{.., copilot}
let r = iuredesk_mcp::register_mcp_agents(true);    // escribe mcpServers.iuredesk en Claude Desktop; abre vscode:mcp/install?…
// r.claude_files, r.claude_error, r.vscode_link, r.vscode_opened
iuredesk_mcp::unregister_mcp_agents()?;              // «Desconectar»
```

Registra el ejecutable actual (`agents::command()`, o el AppImage) con `--mcp`: llamarlo desde
la app instalada. Si `stale`, mostrar «Actualizar conexión» (la app se movió). Sin Claude ni VS
Code: enlaces de descarga (`status.download_url`).

## Pendiente

- Cablear todo en la UI y en `main` (arriba).
- `iurevoz` e IureProject aún no están en el catálogo `apps` de connect (se buscan aquí).
- Herramientas MCP de escritura con deshacer en la UI (hoy el borrador/evento/nota aparece tras
  `on_change`, sin entrada en la pila de deshacer).
- Recurso MCP con el correo de hoy para «Léeme mis correos de hoy» (IureVoz).
