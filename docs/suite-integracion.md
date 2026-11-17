# Integración de la suite: que las iureapps se sientan como una sola

> Análisis, 2026-11-17. Propuesta, no decisión: los puntos marcados **[Dueño]** requieren su
> aprobación porque cambian reglas de `CLAUDE.md` o de otros repositorios.

## Objetivo

Que desde IureDesk se pueda hacer de todo (notas con la calidad de IureEditor, tareas de
IureProject, archivos de IureDav, grabar una junta con IureTranscribe) **sin la percepción de que se
abrieron otras aplicaciones**, sin romper las reglas de la suite: solo Rust en el producto, nunca
fallar, datos externos hostiles, Iurefficient solo por `iurefficient-connect`.

## 1. Punto de partida

| App | Tecnología | ¿Enlazable dentro de IureDesk? | Interfaz hacia IureDesk hoy |
|---|---|---|---|
| **IureProject** | Rust, egui 0.37, `iurefficient-ui` | **Sí, incluso su interfaz** (`iureproject-model`, `-engine`, `-sched`, `-ui-egui`) | MCP: `project_overview`, `schedule_summary`, `task_insert`, `view_gantt`, `iurefficient_cases`, `plan`; abrir archivo |
| **IureEditor** | Tauri (webview + JS) | **No** (webview y JS prohibidos) | Deep links: `iureditor://open`, `insert`, `iurefficient/doc`. Sin MCP ni modo sin ventana |
| **IureTranscribe** | Tauri + `whisper-rs` (whisper.cpp, C++), `panic = "abort"` | **No** (C++; un aborto tumbaría IureDesk) | MCP: `transcribe_file`, `transcription_status`, `read_transcript`, `list_recordings`, `audio_info`, `transcription_setup`. La grabación (`start_recording`, `stop_recording`, `pause_recording`, `recording_status`) solo existe como comando interno de Tauri |
| **IureDav** | Tauri + `iuredav-core` (Rust) + `iuredav-cli` | **Con condiciones**: `keyring` con `sync-secret-service` trae libdbus (C) | Unidades montadas (`DavMount`), guardar adjunto, adjuntos grandes como enlace |
| **IureVoz** | Rust | No hace falta | Socket de control + MCP |

La base ya existe en [`crates/integrations`](../crates/integrations/src/lib.rs): `Suite::detect`,
cliente MCP por stdio con tiempos de espera, deep links, `Outcome::NotInstalled`. Se extiende; no se
reinventa.

## 2. Por qué Mermaid se ve distinto en IureDesk e IureEditor

Hay **tres** renderizadores de Markdown en la suite:

- **IureEditor**: `mermaid.js` (npm `mermaid ^11.16`) en el webview.
- **IureDesk**: crate Rust `mermaid-svg` en [`html-view/src/mermaid.rs`](../crates/html-view/src/mermaid.rs),
  con cerco (código acotado, hilo propio con `catch_unwind`, tiempo máximo, SVG validado), más
  `markdown.rs` y `mindmap.rs`.
- **IureProject**: `egui_commonmark`, sin Mermaid.

El archivo sí es compartido: cada nota de IureDesk es un `.md` que IureEditor edita y que viaja por
IureDav ([`engine/src/notes_fs.rs`](../crates/engine/src/notes_fs.rs)). Lo que difiere es solo el
dibujo. IureDesk no podía usar `mermaid.js` sin un webview.

**Propuesta**: un crate compartido `iure-markdown` (Markdown → HTML/SVG, Mermaid, mapa mental), Rust
puro, extraído de `html-view`. Lo usan:

- IureDesk (`html-view` pasa a depender de él);
- IureProject (sustituye o complementa `egui_commonmark` en sus notas);
- el **backend Rust de IureEditor** (`src-tauri`): un comando `render_mermaid(code, theme) -> svg`
  que el webview usa en lugar de `mermaid.js`.

Resultado: un solo aspecto en toda la suite. **[Dueño]** decidir el renderizador canónico
(recomendación: el de Rust, que ya cumple «nunca fallar»).

## 3. Arquitectura: tres niveles

### Nivel A — Datos y widgets dentro del proceso

Solo para apps en Rust puro con egui 0.37 → en la práctica, **IureProject**.

- Vistas embebidas: «Tareas del proyecto» en el panel de un caso, mini-Gantt, tareas de proyectos en
  «Hoy», con los widgets de `iureproject-ui-egui` dibujados en la ventana de IureDesk.
- Alcance recomendado: **lectura y acciones simples** (ver, marcar hecha, reprogramar, asignar). La
  edición completa del plan va al Nivel C.
- Dependencia por tag fijo de git, como `iurefficient-connect`; registrada en `LAYERS` (L2/L3 para
  model/engine, L6 para ui-egui).
- Costo: versiones de egui amarradas entre los dos repos; tamaño del binario.
- **[Dueño]** cambia la regla «apps hermanas por frontera de proceso» de `CLAUDE.md`. Alternativa sin
  cambio de regla: solo MCP (Nivel B) y dibujar las tareas con controles propios de IureDesk.

### Nivel B — Acciones sin ventana; la interfaz siempre es la de IureDesk

Para todas las apps. Cada una expone un **servidor sin interfaz** y IureDesk dibuja el resultado con
sus propios controles. Encaja con «todo es un comando»: un comando de IureDesk se despacha a una
herramienta remota.

Dos transportes, según el caso:

- **MCP por stdio** (`<app> --mcp`): para trabajo sin estado o de corta duración (consultar,
  convertir, transcribir un archivo). Ya implementado del lado del cliente.
- **Servicio de instancia única con socket de control** (modelo IureVoz): para lo que tiene estado
  compartido o hardware (micrófono, grabación en curso, montajes). Evita que la app gráfica y un
  proceso `--mcp` lanzado por IureDesk se peleen por el mismo recurso.

### Nivel C — Pasar a la app completa (deep link)

Cuando el usuario quiere la herramienta entera: editar el Gantt, un documento largo, configurar
modelos de IureTranscribe. Las tareas de «Hoy» que vienen de Iurefficient abren `iureproject://…` si
está instalada, o la URL web si no. Si la app no está instalada: `Outcome::NotInstalled` con
invitación a instalarla, nunca un error.

**Regla**: un deep link solo **abre o muestra**; nunca ejecuta una acción destructiva ni con efecto,
porque cualquier programa local puede dispararlo.

## 4. Casos concretos

### 4.1 Grabar una junta sin abrir IureTranscribe

1. En el evento del calendario (o en la barra durante una reunión): botón **● Grabar**.
2. IureDesk pide a IureTranscribe, por socket, `start_recording` con los participantes del evento
   como nombres de hablantes.
3. Píldora persistente en la barra de IureDesk: «● Grabando 12:04 ⏸ ⏹» (siempre visible: grabar sin
   indicador es un problema legal y de confianza).
4. Al parar: `transcribe_file` → seguimiento con `transcription_status` (ya existe en
   [`integrations/src/transcribe.rs`](../crates/integrations/src/transcribe.rs)) → la transcripción
   se crea como **nota vinculada al evento** (y al caso de Iurefficient, si lo tiene). Opcional:
   resumen y tareas con el asistente.
5. Deshacer: borrar la nota y la grabación (a la papelera).

### 4.2 Archivos (IureDav) como sección de IureDesk

- Ícono «Archivos» en la barra lateral, visible solo si IureDav está instalada o hay unidades.
- Explorador nativo en egui sobre la unidad montada (no requiere nada nuevo): navegar, abrir, adjuntar
  a un correo arrastrando, guardar adjuntos.
- Acciones de servidor (compartir, enlace público, versiones, espacio) por `iuredav-cli` o
  `iuredav --mcp`.
- Enlazar `iuredav-core` directamente solo si el llavero queda detrás de una *feature* y IureDesk le
  pasa sus credenciales (IureDesk ya tiene llavero con zbus).

### 4.3 Notas con IureEditor «por atrás»

- Render: resuelto por `iure-markdown` (§2).
- Edición rápida: el editor de notas de IureDesk.
- Edición rica (tablas complejas, PDF, plantillas): Nivel C, `iureditor://open?path=…`; la nota se
  relee sola al guardarse (vigilancia de `notes_fs`).
- Conversión sin ventana (exportar a PDF/DOCX, plantillas): requiere `iureditor --mcp`. Nota honesta:
  la edición WYSIWYG de IureEditor *es* su ventana; no se puede hacer invisible sin reescribirla.

### 4.4 Tareas de proyectos en «Hoy»

- Fuente: IureProject (archivos locales, por MCP `schedule_summary` o Nivel A) y Iurefficient (por
  `iurefficient-connect`).
- Cada fila indica su origen; clic → panel de detalle dentro de IureDesk; «Abrir en IureProject» o
  «Abrir en Iurefficient» como acción secundaria.

## 5. Seguridad

- **Mismo proceso = mismo destino.** Lo que contiene C/C++ o webview se queda en otro proceso (además
  lo exige la regla de pureza).
- **Todo lo que llega por MCP o socket es hostil**: tamaños acotados, tiempos de espera, sin `[i]`,
  bucles con límite (regla ya vigente).
- **Sockets locales**: solo el usuario dueño (permisos `0600` en Unix, ACL del pipe en Windows) y un
  token por sesión escrito en un archivo privado, para que otro programa local no pueda ordenar
  «grabar» o «compartir».
- **Acciones con efecto visibles**: grabación, compartir, borrar siempre se ven en la interfaz de
  IureDesk y tienen deshacer cuando es posible.
- **Llamadas programáticas nunca abren diálogos** (regla vigente); si la app hermana necesita algo del
  usuario, devuelve un estado que IureDesk muestra.

## 6. Contrato común de la suite (a añadir en `docs/integraciones.md`)

1. Cada app: `--mcp` (stdio) y, si maneja estado o hardware, servicio de instancia única con socket.
2. Nombres de herramientas en `snake_case`, verbo + objeto, iguales en todas las apps
   (`start_recording`, `list_files`, `render_markdown`…).
3. Errores con mensaje humano bilingüe (`en`/`es`) en el resultado.
4. Deep links: solo abrir/mostrar.
5. App no instalada: `Outcome::NotInstalled` + invitación; nunca error.
6. Versión del contrato en `initialize` (`serverInfo`) para que IureDesk degrade con elegancia.

## 7. Cambios por repositorio

| Repositorio | Cambio | Prioridad |
|---|---|---|
| nuevo `iure-markdown` (o crate en `iurefficient-ui`) | Extraer Markdown + Mermaid + mapa mental de `html-view` | 1 |
| iuredesk | `html-view` usa `iure-markdown` | 1 |
| iureditor | Comando Tauri `render_mermaid` con `iure-markdown`; quitar `mermaid.js` | 1 |
| iuretranscribe | Servicio de instancia única + socket; herramientas `start_recording`, `stop_recording`, `pause_recording`, `recording_status` | 2 |
| iuredesk | Botón Grabar en eventos, píldora en la barra, nota con transcripción vinculada | 2 |
| iuredesk | Sección «Archivos» sobre unidades montadas | 3 |
| iuredav | `--mcp` (o ampliar `iuredav-cli`): compartir, enlaces, versiones; llavero detrás de *feature* | 3 |
| iuredesk | Tareas de IureProject en «Hoy» y en el panel del caso (MCP) | 4 |
| iureproject | Estabilizar API pública de `model`/`engine`/`ui-egui` si se aprueba el Nivel A | 4 |
| iureditor | `--mcp` para exportar/convertir sin ventana | 5 |
| iureproject | Usar `iure-markdown` en sus notas | 5 |
| iuredesk | Documentar el contrato común (§6) en `docs/integraciones.md` | 1 |

## 8. Decisiones pendientes del dueño

1. **Renderizador canónico de Markdown/Mermaid** para toda la suite (recomendación: el de Rust).
2. **¿IureDesk puede enlazar crates de IureProject** (y su interfaz egui) en lugar de hablar solo por
   proceso? (recomendación: sí, solo lectura y acciones simples, por tag fijo).
3. **Dónde vive `iure-markdown`**: crate propio, o dentro de `iurefficient-ui` (que ya comparten
   IureDesk e IureProject).
