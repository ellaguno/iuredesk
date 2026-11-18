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

### 4.5 Destino: IureEditor 2.0 en Rust + egui (`iure-doc`)

Hoy IureEditor es ~22 000 líneas de TS/React sobre Tauri. Su valor está en bibliotecas web maduras:
TipTap/ProseMirror (edición enriquecida), `mermaid.js`, KaTeX, `pagedjs` (PDF), `docx` (npm),
highlight.js. Rehacerlo con Rust + egui 0.37 + `iurefficient-ui` es el único camino para que el
editor se **embeba** en IureDesk (notas, redactor, documentos de un caso) y para tener un solo motor
de documentos en la suite.

**Qué se gana**
- Embebible: el editor es un widget que IureDesk (e IureProject) dibujan en su propia ventana.
- Un solo render de Markdown/Mermaid: S1.2, S1.3 y S1.9 se resuelven por construcción.
- Pureza y «nunca fallar» también en IureEditor; fuera Node/npm y ~40 paquetes JS.
- Un solo stack: ui-kit, temas, densidades, paleta, AccessKit.
- Arranque casi instantáneo y del orden de 50–100 MB de memoria en vez de 200–400 MB (webview).
  PDF con Typst (Rust puro) más rápido y fiel que `pagedjs`.

**Qué no se gana por sí solo**
- Velocidad al escribir: TipTap ya es fluido. egui recalcula la disposición cada cuadro; un documento
  largo sería **más lento** sin caché de disposición por bloque y virtualización (solo dibujar lo
  visible). Es trabajo explícito y medible (meta: < 8 ms por cuadro con 100 páginas).

**Equivalencias**

| Pieza | Hoy | Rust puro |
|---|---|---|
| Edición enriquecida por bloques | TipTap/ProseMirror | **No existe en egui**: se construye (`DocEditor`) |
| Modelo de documento | ProseMirror | `compose::rich::RichBody` ampliado (ya hay ida y vuelta Markdown/HTML) |
| Mermaid | `mermaid.js` | `mermaid-svg` (ya en `html-view`) |
| LaTeX | KaTeX | Opciones inmaduras (prioridad P2, §4.7); evaluar crates y, si no, render propio limitado o fórmula como imagen vía Typst |
| PDF | `pagedjs` | **Typst** (crates `typst`/`typst-pdf`) |
| DOCX | `docx` (npm) | `docx-rs` |
| Resaltado de código | highlight.js | `syntect` con `fancy-regex` (verificar pureza); `tree-sitter` no (C) |

**El reto: el editor de bloques.** Cursor y selección entre bloques, IME (acentos, teclados
asiáticos), deshacer por transacciones, tablas editables, imágenes y diagramas en línea, atajos de
Markdown al teclear, pegar desde web/Word, accesibilidad, corrector ortográfico. Es el corazón del
riesgo y por eso se valida primero dentro de IureDesk.

**Plan incremental** (cada paso entrega algo útil; si el editor no alcanza la calidad de TipTap se
descubre en las notas de IureDesk, no tras retirar IureEditor):

1. **Crate `iure-doc`** (Rust puro, sin egui en el núcleo): modelo de bloques a partir de
   `compose::rich` ampliado con tablas, listas de tareas, Mermaid, fórmulas e imágenes; Markdown
   como formato de archivo (sin pérdidas en lo que IureEditor ya escribe, incluido el YAML de
   cabecera); render compartido (lo que §2 llama `iure-markdown`). Pruebas de propiedades de ida y
   vuelta.
2. **Widget `DocEditor`** (módulo egui de `iure-doc`, o en `iurefficient-ui`): edición por bloques,
   atajos Markdown al teclear, caché por bloque y virtualización. Primer usuario: **las notas de
   IureDesk**; después el redactor de correo (sustituye al editor actual de `compose`).
3. **Exportación**: PDF con Typst, DOCX con `docx-rs`, HTML autocontenido.
4. **IureEditor 2.0**: ventana egui + `iure-doc` + deep links y `--mcp` (S1.8) con los mismos
   esquemas que hoy; migración de preferencias; la versión Tauri se retira cuando haya paridad en
   las funciones que la gente usa.

Mientras tanto, S1.3 (Mermaid de Rust en el backend de Tauri) queda como **paso intermedio** opcional
para unificar el aspecto antes de que llegue la 2.0.

### 4.6 `iure-doc` como formato de documento de la suite

Con `iure-doc` el editor deja de ser «de IureEditor»: es un componente que cualquier app de escritorio
en Rust + egui embebe. Notas, minutas, transcripciones, documentos de casos y notas de tareas se
editan igual en todas.

**Dónde se embebe**

| App | Uso | Cómo |
|---|---|---|
| IureDesk | Notas, redactor, minutas de eventos, notas de casos | Widget `DocEditor` |
| IureProject | Notas por tarea, hito y proyecto (hoy `egui_commonmark`, solo lectura) | Widget `DocEditor`; Markdown dentro del archivo del proyecto (formato sin cambios) |
| IureEditor 2.0 | Documentos completos | §4.5 |
| Iurefficient web | Notas y documentos por `iurefficient-connect` | No embebe el widget; el Markdown es el mismo y se ve/edita igual en escritorio |
| IureTranscribe | Edición de transcripciones | Ver abajo |

**IureTranscribe** (Tauri + whisper.cpp) no puede alojar un widget egui de forma natural:

1. **Recomendado, inmediato**: la transcripción llega a IureDesk (S1.4) o a IureEditor 2.0 como
   documento `iure-doc` y se edita ahí; el usuario no abre IureTranscribe.
2. **Destino**: IureTranscribe con interfaz egui + `DocEditor`, y whisper.cpp en un **proceso
   aparte** (servicio de S1.4). La interfaz queda en Rust puro y un aborto del motor C++ no tumba la
   ventana. Mucho menos frontend que reescribir que IureEditor.
3. Descartado: `DocEditor` compilado a WASM dentro del webview (IME, accesibilidad y selección
   peores en canvas que el editor web nativo).

**Requisitos para `iure-doc`** (desde S1.11):

- **Bloques extensibles registrados por el anfitrión**: segmento de transcripción (hablante,
  inicio/fin, clic → reproducir en ese punto), referencia a tarea (estado en vivo de IureProject),
  vínculo a correo (abrir en IureDesk). El núcleo los guarda y **preserva aunque una app no sepa
  dibujarlos** (se muestran como bloque genérico y vuelven intactos al archivo).
- **Acciones provistas por el anfitrión**, nunca por el editor: abrir enlace, reproducir audio en
  `t`, buscar contacto para @mencionar, subir imagen. El editor no hace E/S (como `html-view`).
- **Markdown con metadatos sin pérdidas**: tiempos y hablantes sobreviven a la edición (borrar una
  frase no desalinea las demás). Metadatos por bloque (atributos en YAML o comentarios
  estructurados), con pruebas de propiedades de ida y vuelta y lectura tolerante de archivos
  editados a mano en otro editor.

### 4.7 Calidad exigida: edición enriquecida y diagramas

Principio: **lo que un usuario usa a diario debe funcionar «muy bien»**, al nivel de TipTap/Notion o
mejor; lo raro puede ser sencillo, pero nunca rompe el documento. LaTeX es prioridad baja.

#### Prioridades por uso

| Nivel | Qué | Exigencia |
|---|---|---|
| **P0 — diario** | Párrafos, títulos 1–3, negrita, cursiva, tachado, código en línea, enlaces, listas con viñetas / numeradas / de tareas con anidado, citas, bloques de código con lenguaje, separador, imágenes, tablas sencillas, deshacer/rehacer, copiar/pegar, buscar y reemplazar, IME y acentos, guardado automático | Impecable. Sin saltos de cursor, sin pérdidas, < 8 ms por cuadro |
| **P1 — frecuente** | Avisos (`> [!NOTE]`…), Mermaid, mapas mentales, resaltado, color de texto, alineación, notas al pie, títulos 4–6, comandos `/` | Muy bien, con opciones de dibujo (abajo) |
| **P2 — ocasional** | Fórmulas LaTeX, subíndice/superíndice, tablas complejas (celdas combinadas) | Funcional; lo no soportado se muestra como código con formato, nunca como error que rompa la nota |

#### Markdown al teclear (reglas de entrada)

Al escribir al inicio de un bloque o al cerrar una marca, el texto se convierte al momento:

| Se teclea | Resultado |
|---|---|
| `# ` … `###### ` | Título 1–6 |
| `- ` `* ` `+ ` | Lista con viñetas |
| `1. ` `1) ` | Lista numerada (respeta el número inicial) |
| `[ ] ` `[x] ` (también dentro de una lista) | Lista de tareas |
| `> ` | Cita |
| `> [!NOTE] ` `[!TIP]` `[!WARNING]` `[!MINDMAP]`… | Aviso / mapa mental |
| ```` ``` ```` + lenguaje + Enter | Bloque de código (```` ```mermaid ```` → diagrama) |
| `---` `***` + Enter | Separador |
| `**texto**` `__texto__` | Negrita |
| `*texto*` `_texto_` | Cursiva (sin falsos positivos en `snake_case` ni en `2*3*4`) |
| `~~texto~~` | Tachado |
| `` `texto` `` | Código en línea |
| `==texto==` | Resaltado |
| `[texto](url)` | Enlace |
| URL o correo seguido de espacio | Enlace automático |
| `\| a \| b \|` + Enter | Tabla con encabezado |
| `$…$` | Fórmula (P2) |

Comportamientos que distinguen un editor bueno de uno excelente:

- **Retroceso justo después de una conversión la deshace** y deja el texto literal (`# ` vuelve a
  ser `# `). `Ctrl+Z` hace lo mismo. Es la salida natural cuando el usuario no quería formato.
- Nada se convierte dentro de código (bloque o en línea) ni en un enlace.
- **Enter inteligente**: en un elemento de lista vacío sale de la lista; en un título continúa con
  párrafo; en una tarea crea otra tarea sin marcar; en código inserta salto (dos Enter vacíos al
  final salen del bloque).
- `Tab` / `Shift+Tab` sangran y desangran elementos de lista; en tabla, celda siguiente/anterior
  (Tab en la última celda agrega fila).
- Los atajos `Ctrl+B/I/U/K`, `Ctrl+Shift+7/8/9` (listas), `Ctrl+Alt+1…3` (títulos) funcionan y se
  ven en la paleta.
- **Pegar**: Markdown pegado se convierte; HTML de web, Word, LibreOffice, Google Docs y correos se
  limpia a lo que el modelo soporta (sin estilos basura); `Ctrl+Shift+V` pega como texto plano.
- Tipografía opcional (preferencia): `--` → —, `...` → …, comillas tipográficas según idioma.
- Comandos `/` (título, lista, tabla, diagrama, mapa mental, aviso, imagen, fecha) y vista de código
  fuente (`Ctrl+/`) que alterna sin perder la posición del cursor.

**Fidelidad del archivo**: un bloque que el usuario no tocó se escribe **byte por byte igual** que se
leyó (sin reformatear `*` por `-`, sangrías ni saltos). Así los diffs de git son limpios, IureEditor
actual y otros editores conviven, y los metadatos ajenos sobreviven.

#### Diagramas: opciones de dibujo

`mermaid-svg` 0.7 ya dibuja 23 tipos (flujo, secuencia, Gantt, clase, ER, estado, mapa mental
radial, línea de tiempo, pastel, kanban, C4, arquitectura, treemap…) y respeta el `config:` de
Mermaid (frontmatter y `%%{init}%%`), `themeVariables`, `accTitle`/`accDescr`. Se aprovecha todo y
se enriquece:

- **Mapas mentales con distribución a elegir**: *radial* (como IureEditor hoy; es el dibujo nativo
  de `mermaid-svg`), *a la derecha* (como IureDesk hoy), *equilibrado a ambos lados* y *de arriba
  abajo* (organigrama). Además las **formas de nodo** que IureEditor ya guarda (`default`, `square`,
  `rounded`, `circle`, `cloud`, `bang`, `hexagon`), que IureDesk hoy ignora.
- **Dónde se guarda la opción**, compatible con lo que ya existe:
  - mapas `> [!MINDMAP|…]`: nuevos elementos del metadato (p. ej. `|map radial cloud`), sin romper
    `map`, `right`, `links` ni la forma; lo desconocido se preserva;
  - bloques ```` ```mermaid ````: el `config:` estándar de Mermaid (frontmatter), para que
    `mermaid.js` y cualquier otro visor lo entiendan; las opciones propias van bajo una clave
    `iure:` que los demás ignoran.
- **Valor por omisión** en preferencias (por tipo de diagrama) y **cambio rápido por diagrama** con
  una barra flotante al pasar el ratón: distribución ▾, forma ▾, tema ▾, editar código.
- **Tema** que sigue al de la app (claro/oscuro, colores de `iurefficient-ui` traducidos a un
  `Theme`), con opción de los temas de Mermaid (`default`, `forest`, `neutral`…).
- **Editar el código con vista previa en vivo** y, si hay error, la línea marcada con mensaje en
  lenguaje humano; el último dibujo válido se mantiene mientras se corrige.
- **Zoom y desplazamiento** para diagramas grandes; **exportar** a SVG y PNG; nodos con `click`
  despachan acciones del anfitrión (§4.6), nunca abren URLs sin confirmar.
- **Corpus de fidelidad**: los tipos más usados (flujo, secuencia, Gantt, mapa mental, pastel, clase,
  ER, línea de tiempo) se comparan con capturas de referencia de `mermaid.js`; las diferencias que
  importan se corrigen.
- **Enriquecer `mermaid-svg`** (MIT): las mejoras generales (distribuciones de mapa mental, ganchos
  de `click`, métricas de texto con la fuente real) se proponen **aguas arriba** como PR; solo si no
  se aceptan, se mantiene una capa propia en `iure-doc`. Se fija la versión exacta y se actualiza
  con el corpus como red.

#### Medido en la primera entrega (2026-11-17)

`DocEditor` como un solo `TextEdit` con estilo al teclear: una nota normal (< 20 KB) se repinta
en ~3 ms; 250 KB (~100 páginas) en ~40 ms al teclear (~20 ms de estilo + ~19 ms de glifos de egui,
que vuelve a maquetar todo el texto en cada tecla). Para la meta de < 8 ms con 100 páginas el
editor de IureEditor 2.0 debe ser **por bloques**: un campo por bloque, caché por bloque y solo lo
visible en pantalla; la selección entre bloques se resuelve por encima.

Hecho en la cuarta entrega (`iure_doc_ui::show_blocks`, ya en las notas de IureDesk): 250 KB
cuestan **~1,1 ms por tecla** (rehacer el bloque editado y dibujar lo visible) y 19 ms al abrir.
Falta, para la paridad con un solo campo: selección que cruce bloques y deshacer de todo el
documento.

#### Aceptación

- Corpus de documentos reales de IureEditor: abrir y guardar sin tocar → archivo idéntico.
- Guion de pruebas `egui_kittest` por cada regla de entrada y cada comportamiento de arriba
  (incluido «retroceso deshace la conversión»).
- Pruebas de propiedades: Markdown → modelo → Markdown estable; texto hostil acotado sin pánico.
- Prueba de rendimiento: documento de 100 páginas con 20 diagramas, < 8 ms por cuadro al teclear.
- Lista de paridad frente a IureEditor actual (TipTap) firmada antes de S1.15.

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
| nuevo `iure-doc` | Modelo de bloques + Markdown sin pérdidas + render compartido (absorbe `iure-markdown`) | 2 |
| iuredesk | Notas y luego redactor sobre el widget `DocEditor` | 3 |
| `iure-doc` | Exportar PDF (Typst), DOCX (`docx-rs`), HTML | 4 |
| iureditor | IureEditor 2.0: egui + `iure-doc`; retirar Tauri al lograr paridad | 5 |
| iureproject | Notas enriquecidas por tarea/hito/proyecto con `DocEditor` | 4 |
| iuredesk | Transcripción editable (bloques de segmento, reproducir en el punto) | 3 |
| iuretranscribe | Interfaz egui + `DocEditor`; whisper.cpp como proceso aparte | 5 |
| iuredesk (`html-view`) | Opciones de mapa mental (distribución y formas) ya en la vista de notas actual | 2 |
| `iure-doc` / mermaid-svg | Barra de diagrama, tema de la app, vista previa en vivo, zoom, exportar, corpus de fidelidad; PR aguas arriba | 3 |

## 8. Decisiones del dueño (2026-11-17)

1. **Renderizador canónico**: el de Rust (`mermaid-svg` + dibujo propio de mapas). IureEditor lo
   adoptará con la 2.0; mientras tanto S1.3 queda opcional.
2. **IureProject solo por MCP** (no se enlazan sus crates; se mantiene la frontera de proceso).
3. **Dónde vive `iure-doc`**: empezó como crate del workspace de IureDesk (`crates/doc` y
   `crates/doc-ui`); desde 2026-11-18 tiene repositorio propio,
   [`ellaguno/iure-doc`](https://github.com/ellaguno/iure-doc) (decisión del dueño), y las apps lo
   usan por etiqueta fija (`v0.1.0`).
4. **IureEditor 2.0 en Rust + egui, por pasos**: `iure-doc` → `DocEditor` en las notas de
   IureDesk → redactor → exportar → IureEditor 2.0. IureEditor Tauri sigue hasta la paridad.
5. **IureTranscribe con interfaz egui** y whisper.cpp en proceso aparte: **ya** (plan en su repo).
6. **Permisos**: PR a `mermaid-svg` y navegador sin pantalla en desarrollo para las referencias
   de `mermaid.js` (S1.21).
