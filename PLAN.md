# IureDesk — Plan maestro

> Gestor de información personal (PIM) de escritorio, 100 % Rust, para Linux, macOS y Windows.
> Correo · Calendario · Contactos · Notas · Tareas · Colaboración · IA de Iurefficient.
>
> Estado: **v0.2 (2026-10-26)** — decisiones de §13 tomadas; F0 en curso.

---

## 1. Visión

IureDesk debe poder **sustituir por sí solo** a Outlook, Thunderbird y Apple Mail + Calendario para
cualquier persona, y al mismo tiempo ser **la puerta de entrada de escritorio al ecosistema
Iurefficient** (IureEditor, IureDav, IureTranscribe, IureVoz, IureProject, IureOCR).

Tres promesas que guían cada decisión:

1. **Funciona sin Iurefficient.** Una cuenta IMAP/Gmail/Microsoft 365/iCloud basta para tener un
   PIM completo. Iurefficient *suma*, no es requisito.
2. **Cualquier usuario.** Se configura escribiendo correo y contraseña (o "Iniciar sesión con…").
   Sin jerga, sin ribbon, sin diálogos de 40 opciones. Lo avanzado existe, pero escondido.
3. **Densidad de escritorio.** Ver más en menos espacio que cualquier cliente web: listas compactas,
   tres paneles, teclado primero, 60 fps, arranque < 1 s, < 150 MB de RAM con 100 k mensajes.

### Lo que NO es

- No es un clon visual de Outlook ni de IureProject (que apunta a PM *power users*).
- No es un cliente web empaquetado: nada de webview, Electron ni Tauri.

---

## 2. Decisiones de arquitectura (propuestas)

| # | Decisión | Propuesta | Por qué |
|---|----------|-----------|---------|
| D1 | Toolkit UI | **egui/eframe 0.37 + wgpu** | Mismo toolkit que IureProject (0.36) e IureVoz (0.37): compartimos tema, iconos, paleta de comandos y tests `egui_kittest`. El modo inmediato encaja con listas densas y virtualizadas. |
| D2 | Pureza | **Regla de IureProject**: sin `-sys` de C/C++, sin webview, sin JS | Binario único, reproducible, portable; `cargo xtask purity` lo vigila. Excepciones documentadas en `docs/excepciones.md`. |
| D3 | Render de correo HTML | **Blitz** (motor HTML/CSS en Rust puro de Dioxus: `blitz-dom` + `blitz-html` + `stylo`) pintando a textura dentro de egui; *fallback* a texto enriquecido propio | Es la única vía de mostrar HTML fiel sin webview. Ventaja de seguridad: **no ejecuta JavaScript**. Riesgo principal del proyecto (ver §12). |
| D4 | Almacenamiento | **redb** (metadatos, índices, estado de sync) + **blobs comprimidos zstd** (RFC 822 crudos, adjuntos, `.ics`, `.vcf`) | Igual que IureProject (redb, no SQLite). Los blobs crudos permiten re-parsear sin re-descargar. |
| D5 | Búsqueda | **tantivy** | Búsqueda instantánea local, con operadores (`de:`, `adjunto:`, `antes:`). |
| D6 | Asincronía | **tokio** en un *runtime* de fondo; la UI nunca bloquea; comunicación por canales | La UI de egui sólo lee *snapshots* del modelo. |
| D7 | Conexión Iurefficient | **iurefficient-connect** (crate compartido) | Login, TOTP, llavero compartido, REST, MCP, WebDAV y CRM ya existen. Lo que falte (agenda, contactos, tickets) se agrega **ahí**, no en IureDesk. |
| D8 | Offline-first | Todo se lee del almacén local; la red sólo sincroniza | Velocidad, trabajo en avión, resiliencia ante servidores lentos. |
| D10 | Licencia | **Apache-2.0** (2026-10-28; antes FSL-1.1-ALv2) | Repositorio y releases públicos en el mismo repo; IureProject sigue en FSL. |
| D11 | ui-kit compartido | **`iurefficient-ui`** (repo hermano) extraído de IureProject; IureProject migra a él | Una sola apariencia y un solo lugar para mejorar widgets. |
| D12 | Versión de egui | **0.37 unificada** en toda la suite | |
| D9 | Integración con apps hermanas | **A través de frontera de proceso** (deep links, MCP, socket de IureVoz) | Misma decisión que PLAN D3 de IureProject: cada app evoluciona sola; IureDesk no las enlaza como librería salvo `connect`. |

---

## 3. Stack en Rust puro

| Área | Crate(s) | Notas |
|------|----------|-------|
| UI | `eframe`, `egui`, `egui_extras`, `egui_kittest` | wgpu; Wayland + X11; AccessKit |
| HTML de correo | `blitz-dom`, `blitz-html`, `blitz-paint`/`anyrender` | Imágenes remotas bloqueadas por defecto |
| TLS | `rustls`, `tokio-rustls`, `rustls-platform-verifier` | Sin OpenSSL |
| IMAP | `imap-next` + `imap-codec` (o `async-imap` con rustls) | IDLE, CONDSTORE/QRESYNC, MOVE, UIDPLUS, COMPRESS |
| POP3 | módulo propio (protocolo pequeño) | Con "dejar copia en servidor" |
| SMTP | `lettre` (rustls, tokio) | XOAUTH2 / OAUTHBEARER |
| MIME | `mail-parser`, `mail-builder` (Stalwart) | Robustos ante correos mal formados |
| OAuth 2 | `oauth2` + servidor *loopback* local + PKCE | Google, Microsoft |
| Microsoft 365 | cliente Graph propio sobre `reqwest` | Correo, calendario, contactos, To Do. **EWS se retira en Exchange Online**: no se implementa. |
| Google | Gmail API (opcional, etiquetas nativas), Calendar API, People API — o IMAP/CalDAV/CardDAV | Ver riesgo de verificación de Google (§12) |
| CalDAV/CardDAV | `libdav` (Pimalaya) o cliente propio sobre `quick-xml` | iCloud, Fastmail, Nextcloud, Iurefficient, etc. |
| iCalendar/vCard | `calcard` (Stalwart) o `icalendar` + `rrule` | Recurrencias, excepciones, iTIP/iMIP |
| Zonas horarias | `chrono` + `chrono-tz` (coherente con IureProject) | |
| Almacén | `redb`, `zstd` (puro: `ruzstd` si `zstd-sys` no pasa purity) | |
| Búsqueda | `tantivy` | |
| Secretos | `keyring` 3 (secret-service vía zbus / windows-native / apple-native) | Llavero compartido `iurefficient` |
| Notificaciones | `notify-rust` (zbus) en Linux; WinRT en Windows; excepción documentada en macOS | |
| Bandeja | `ksni` en Linux (como IureVoz); nativo por SO en el resto | |
| Diálogos de archivo | `rfd` con portal XDG | |
| Importación | lectores mbox / Maildir / `.emlx` propios; PST: evaluar `outlook-pst` | Migración desde Outlook, Thunderbird, Apple Mail |
| Ortografía | `spellbook` (Hunspell en Rust puro) | Diccionarios es/en |

---

## 4. Arquitectura en capas

Mismo esquema L1–L6 que IureProject, verificado por `cargo xtask layers`. **Nada por debajo de L6
depende de egui.**

```
L6  apps/iuredesk            binario egui (+ --mcp, deep links, mailto:)
    apps/iuredesk-cli        CLI: sync, buscar, enviar, importar, exportar
──────────────────────────────────────────────────────────────────────
L5  crates/ui                vistas: correo, calendario, contactos, notas, tareas, IA
    crates/ui-kit            tema, iconos, lista virtual densa, paleta de comandos
                             (extraído/compartido con IureProject)
    crates/html-view         puente Blitz → textura egui, saneado, bloqueo remoto
──────────────────────────────────────────────────────────────────────
L4  crates/engine            Session, comandos (todo es un comando), deshacer,
                             reglas/filtros, envío programado, i18n
    crates/assistant         IA: redactar, resumir, minutas, triage, acciones
    crates/integrations      apps hermanas (IureEditor, IureDav, IureTranscribe,
                             IureVoz, IureProject, IureOCR) vía deep links / MCP
──────────────────────────────────────────────────────────────────────
L3  crates/sync              orquestador: colas, reintentos, conflictos, IDLE/push
    crates/provider-*        imap, pop3, smtp, graph, google, dav, iurefficient
──────────────────────────────────────────────────────────────────────
L2  crates/store             redb + blobs + tantivy, migraciones
    crates/autoconfig        ISPDB de Thunderbird, Autodiscover, SRV (RFC 6186/6764)
──────────────────────────────────────────────────────────────────────
L1  crates/model             Account, Folder, Message, Thread, Event, Contact,
                             Note, Task, Attachment, Identity (sin E/S)
    crates/mime, ical        parseo/serialización pura
```

### Flujo de datos

```
 Servidor ──provider──▶ sync ──▶ store (redb/blobs/tantivy) ──snapshot──▶ engine ──▶ ui
                         ▲                                                 │
                         └──────────── cola de operaciones (offline) ◀─────┘
```

Toda acción del usuario (mover, marcar, enviar, aceptar invitación) se escribe **primero** en la
cola local y la UI se actualiza al instante; `sync` la replica cuando hay red, con deshacer real.

---

## 5. Matriz de proveedores

| Proveedor | Correo | Calendario | Contactos | Notas | Tareas | Auth |
|-----------|--------|------------|-----------|-------|--------|------|
| **Genérico** | IMAP / POP3 + SMTP | CalDAV | CardDAV | Local / WebDAV | CalDAV VTODO | Contraseña, autoconfig |
| **Iurefficient** | — (no provee buzones; vincula correos de otras cuentas) | Agenda/eventos de la instancia | Contactos/CRM | Documentos (WebDAV) | Tareas de casos | `iurefficient-connect` (cookies + TOTP) |
| **Gmail / Workspace** | IMAP+XOAUTH2 o Gmail API | Calendar API / CalDAV | People API / CardDAV | Local | Tasks API | OAuth 2 (PKCE) |
| **Microsoft 365 / Outlook.com** | Graph (o IMAP+OAuth) | Graph | Graph | OneNote: fuera de alcance v1 | To Do vía Graph | OAuth 2 (MSAL-like, cliente público multiinquilino) |
| **Apple iCloud** | IMAP + SMTP | CalDAV | CardDAV | IMAP "Notes" (sólo lectura) | CalDAV VTODO (Recordatorios antiguos) | Contraseña específica de app |

Asistente de alta: el usuario escribe su correo → detectamos proveedor (dominio, MX, autoconfig,
Autodiscover) → mostramos **un solo botón** ("Continuar con Google", "Continuar con Microsoft",
"Contraseña de app de Apple — te muestro cómo") → se activan correo, calendario y contactos de
golpe.

---

## 6. UX: "cualquier usuario" con alta densidad

### Distribución principal

```
┌──┬────────────────┬──────────────────────────┬──────────────────────────┐
│✉ │ Bandeja unif.  │ ● Ana López      10:42   │ Re: Propuesta Q4          │
│📅│  Entrada   12  │   Propuesta Q4 — Te en…  │ Ana López · hoy 10:42     │
│👤│  Destacados    │   Juan Pérez     09:15   │ ─────────────────────────│
│📝│  Enviados      │   Factura #391 — Adjun…  │ (cuerpo HTML con Blitz)   │
│✓ │  Borradores    │ ▸ Equipo (4)     ayer    │                           │
│  │ ───────────    │   Minuta lunes — …       │ ┌ Iurefficient ─────────┐ │
│✦ │ Cuenta Gmail   │                          │ │ Lead: ACME · $120k    │ │
│  │ Cuenta Iure    │                          │ │ [Vincular] [Resumir]  │ │
│⚙ │ Hoy: 3 citas   │                          │ └───────────────────────┘ │
└──┴────────────────┴──────────────────────────┴──────────────────────────┘
 riel   carpetas        lista virtual densa        lectura + panel contextual
```

- **Riel de módulos** fijo (Correo, Calendario, Contactos, Notas, Tareas, Asistente ✦).
- **Panel "Hoy"** siempre visible al pie de las carpetas: próximas citas y tareas (como el
  *To-Do bar* de Outlook, pero simple).
- **Panel contextual** a la derecha del mensaje: quién es la persona, citas compartidas, casos,
  leads o tickets de Iurefficient vinculados y acciones de IA. Se oculta si no aporta.

### Reglas de diseño

1. **Tres densidades** (Cómoda / Compacta / Densa) con un clic; la Densa supera a Outlook
   (filas de 22 px, una línea, columnas configurables).
2. **Sin ribbon.** Barra de acciones contextual de ≤ 6 botones + menú "…" + paleta de comandos
   (`Ctrl+K`) que alcanza *todo*.
3. **Atajos compatibles**: esquemas "Outlook", "Gmail" y "Apple Mail" elegibles en el primer
   arranque, para que nadie reaprenda.
4. **Lenguaje humano**: "No se pudo conectar con Gmail — revisa tu internet" en vez de
   `IMAP NO [AUTHENTICATIONFAILED]` (mismo enfoque que `errors` de IureDav).
5. **Deshacer siempre** (borrar, mover, enviar con retraso configurable de 5–30 s).
6. **Accesible**: AccessKit, navegación total por teclado, contraste AA, escalado de fuente.
7. **Bilingüe es/en** desde el día uno (`lang::pick`).
8. Claro / oscuro / sistema con seguimiento en vivo del portal XDG (reutilizar `prefs.rs`).

### Funciones que igualan o superan a Outlook / Apple / Thunderbird

- Bandeja unificada, conversaciones, "Enfocados/Otros" (local, sin IA obligatoria).
- Posponer (snooze), recordatorios de seguimiento, envío programado, plantillas, firmas por identidad.
- Reglas/filtros locales y del servidor (Sieve/ManageSieve cuando esté disponible).
- Búsqueda instantánea con carpetas inteligentes (búsquedas guardadas).
- Calendario: día / semana laboral / semana / mes / agenda / multi-calendario lado a lado,
  arrastrar para crear/mover, asistente de programación con disponibilidad (free/busy),
  invitaciones iMIP aceptables desde el correo, zonas horarias múltiples.
- Contactos unificados con deduplicación, grupos, historial de interacción.
- Arrastrar un correo al calendario → cita; a tareas → tarea; a notas → nota.
- Importar desde Outlook (PST), Thunderbird (mbox), Apple Mail (`.emlx`).
- Cliente predeterminado de `mailto:`, `.eml`, `.ics`, `.vcf`.

---

## 7. Integración con el ecosistema Iurefficient

| App | Cómo se integra IureDesk |
|-----|---------------------------|
| **iurefficient-connect** | Dependencia directa. Cuenta activa compartida (`~/.config/iurefficient/cuenta-activa.json`, llavero `iurefficient`): si el usuario ya inició sesión en otra app de la suite, IureDesk entra sin pedir nada. Se agrega `AppId::IureDesk` al catálogo `apps`. |
| **IureEditor** | "Abrir/editar en IureEditor" para notas y documentos largos (`iureditor://open`, `iureditor://iurefficient/doc`). Redactar un correo largo en IureEditor y volver. |
| **IureDav** | Adjuntos grandes como enlace a WebDAV; "Guardar adjunto en Iurefficient"; mostrar unidades montadas (`apps::iuredav_mounts`). |
| **IureTranscribe** | Desde una cita: "Grabar reunión" → transcripción → **minuta** (blueprint compose) → borrador de correo a los asistentes + tareas desde compromisos. Vía `--mcp` (`transcribe_file`, `read_transcript`). |
| **IureVoz** | Dictado en el redactor; "Léeme mis correos de hoy"; comandos ("responde que sí a Ana"). Vía socket `$XDG_RUNTIME_DIR/iurevoz.sock` y MCP. |
| **IureProject** | Correo → tarea del plan; calendario muestra hitos; reuniones vinculadas a casos. Vía deep link/MCP. |
| **IureOCR** | Adjuntos escaneados → PDF buscable antes de archivar. Vía `--mcp ocr_file`. |

Las funciones de las apps hermanas **se ofrecen sólo si están instaladas** (`apps::detect`). Si no,
aparece un enlace discreto para instalarlas — nunca un error.

### IureDesk como servidor MCP

`iuredesk --mcp` (con `mcp_server::Handler` de connect) expone: `buscar_correo`, `leer_hilo`,
`crear_borrador`, `agenda`, `crear_evento`, `buscar_contacto`, `crear_nota`. Así Claude Desktop,
IureVoz o el propio asistente de Iurefficient pueden actuar sobre el PIM local.

---

## 8. Inteligencia (asistente ✦)

Proveedores (rasgo `AiProvider` en `crates/assistant`):

1. **Iurefficient** (por defecto si hay cuenta): `global_chat`, `ai_options`, `blueprints/compose`,
   `commitments`. Es el único con contexto completo (casos, CRM, tickets).
2. **Microsoft 365 Copilot**, si el inquilino tiene licencia: Copilot Chat API / Retrieval API de
   Microsoft Graph con el mismo token OAuth de la cuenta M365.
3. **Externo configurable**: API compatible con OpenAI (OpenRouter, Ollama/llama.cpp locales) y
   Anthropic. Sin contexto de Iurefficient, sólo el correo/cita en pantalla.

| Función | Dónde aparece |
|---------|---------------|
| Redactar / reescribir (tono, longitud, idioma) | Redactor: botón ✦ y `Ctrl+J` |
| Responder rápido (3 propuestas) | Pie del mensaje |
| Resumir hilo largo | Cabecera del hilo |
| Minuta de reunión | Cita pasada (+ IureTranscribe) |
| Crear documento a partir de correos | Selección múltiple → "Crear documento" → IureEditor / Documentos |
| Analizar correo: intención, fechas, compromisos, montos | Panel contextual |
| **Leads / oportunidades**: detectar, crear o actualizar en CRM, registrar actividad | Panel contextual → `/api/plugins/crm/*` |
| **Tickets**: detectar solicitud de soporte, crear/actualizar ticket, responder con base de conocimiento | Panel contextual (endpoints por confirmar, §13) |
| Triage de la bandeja ("¿qué necesito atender hoy?") | Panel "Hoy" |

Principios: **la IA propone, el usuario confirma** (nada se envía ni se crea en el CRM sin un clic);
se muestra qué datos salen del equipo; se puede desactivar por cuenta.

---

## 9. Seguridad y privacidad

- Contraseñas y tokens sólo en el llavero del SO; nunca en disco plano.
- HTML saneado; **sin JavaScript** (Blitz no lo ejecuta); imágenes remotas bloqueadas por
  defecto (anti-rastreo), permitidas por remitente.
- Detección de phishing básica: dominio del enlace ≠ texto visible, remitentes parecidos.
- S/MIME y OpenPGP (`sequoia-openpgp` con backend Rust puro) en fase posterior.
- Cifrado opcional del almacén local.
- Regla "nunca crashear" de IureProject: sin `unwrap`/`expect`/`panic!` fuera de tests; todo dato
  externo es hostil (tamaños acotados, `get()`, aritmética saturada). Fuzzing de parsers MIME/ICS/IMAP.

---

## 10. Calidad y empaquetado

- `cargo xtask ci`: fmt, clippy `-D warnings`, tests, layers, purity, licenses, notices, assets.
- Servidores de prueba en CI: Stalwart (IMAP/SMTP/CalDAV/CardDAV, Rust) en contenedor; *mocks*
  de Graph y Google.
- Pruebas de UI con `egui_kittest` y capturas *headless* (`--example ui_shot`).
- Benchmarks de rendimiento como criterio de aceptación (arranque, RAM, scroll de 100 k filas).
- Empaquetado con `cargo-packager`: AppImage/.deb/Flatpak, `.msi` firmado, `.dmg` firmado y
  notarizado. Autoactualización vía módulo `releases` de connect.

---

## 11. Hoja de ruta

| Fase | Contenido | Criterio de salida |
|------|-----------|--------------------|
| **F0 · Cimientos** (3–4 sem) | Workspace, xtask (ci/purity/layers), `ui-kit` extraído de IureProject, *shell* con riel + 3 paneles, `model`, `store`, cuentas y llavero vía connect, prototipo Blitz-en-egui | Ventana arranca < 1 s; Blitz pinta un boletín real dentro de egui |
| **F1 · Correo MVP** (8–10 sem) | IMAP (IDLE, QRESYNC), SMTP, POP3, autoconfig, sync offline, lista virtual, conversaciones, lector HTML, redactor básico, adjuntos, búsqueda tantivy, bandeja unificada, notificaciones | Uso diario real con Gmail (contraseña de app), iCloud y un IMAP genérico |
| **F2 · OAuth + Calendario + Contactos** (8–10 sem) | OAuth Google/Microsoft, Graph, CalDAV/CardDAV, vistas de calendario, invitaciones iMIP, free/busy, contactos unificados | Reemplaza Outlook para una cuenta M365 |
| **F3 · Iurefficient** (6 sem) | Sesión compartida, agenda/contactos de la instancia, vincular correo ↔ caso/lead/ticket, guardar adjuntos, comentarios de colaboración, asistente ✦ | Flujo "correo de cliente → lead + respuesta redactada" en 3 clics |
| **F4 · Notas, tareas y ecosistema** (6 sem) | Notas Markdown (local/WebDAV), tareas (VTODO/To Do/Iurefficient), IureTranscribe → minutas, IureVoz, IureEditor, IureOCR, `--mcp` | Reunión → minuta → correo + tareas sin salir de IureDesk |
| **F5 · Paridad y pulido** (8 sem) | Importadores PST/mbox/emlx, reglas, plantillas, envío programado, Sieve, impresión/PDF, S/MIME/PGP, empaquetado 3 SO, accesibilidad | Beta pública |

---

## 12. Riesgos principales

| Riesgo | Impacto | Mitigación |
|--------|---------|------------|
| **Render HTML sin webview** (Blitz aún madura; correos con tablas y CSS antiguo) | Alto | Prototipo en F0 con un corpus de 500 correos reales; *fallback* a vista simplificada; contribuir *upstream*. |
| **Redactor de texto enriquecido** en egui (no existe uno maduro) | Alto | v1: editor propio con formato básico (negrita, listas, enlaces, citas, imágenes en línea) que genera HTML; documentos complejos → IureEditor. |
| **Verificación de Google** para el alcance `https://mail.google.com/` (alcance restringido → auditoría de seguridad CASA anual) | Alto (costo/tiempo) | Empezar con contraseña de app + OAuth en modo "pruebas" (≤ 100 usuarios); presupuestar la auditoría antes de la beta pública. |
| Registro de app en Microsoft Entra (multiinquilino) y consentimiento de administradores | Medio | Cliente público con PKCE; documentación para TI de clientes. |
| Pureza vs. integraciones de SO (notificaciones macOS, bandeja Windows) | Medio | Excepciones acotadas y documentadas en `docs/excepciones.md`. |
| Alcance enorme | Alto | Fases con criterio de salida; cada fase es usable por sí sola. |

---

## 13. Decisiones tomadas (2026-10-26)

1. **Licencia**: FSL-1.1-ALv2 → **Apache-2.0** desde 2026-10-28, repositorio público.
2. **Correo**: Iurefficient no provee buzones. IureDesk usa cuentas propias del usuario y vincula
   correos a casos/leads/tickets.
3. **Endpoints faltantes** (agenda, contactos, tickets, notas): la instancia los expondrá por REST y
   CalDAV/CardDAV; se agregan en `iurefficient-connect`. Más adelante, quizá algo más directo.
4. **IA externa** permitida (sin contexto de Iurefficient). Integrar Microsoft 365 Copilot cuando
   el inquilino lo tenga.
5. **ui-kit** compartido `iurefficient-ui`; IureProject migra a él.
6. **egui 0.37** unificado en toda la suite.
