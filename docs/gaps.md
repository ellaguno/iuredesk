# Lista de trabajo

Ids `F<fase>.<n>`. Estado: ☐ pendiente · ◐ en curso · ☑ hecho.

## F0 · Cimientos

| Id | Tarea | Estado |
|---|---|---|
| F0.1 | Workspace, xtask (ci/layers/purity/licenses/notices/assets/dist), CLAUDE.md, licencia | ☑ |
| F0.2 | `iurefficient-ui`: extraer tema, tokens, iconos, prefs (tema del sistema, fuentes), a11y, paleta de comandos, lista virtual densa de IureProject; egui 0.37 | ☑ |
| F0.3 | `html-view`: prototipo Blitz → textura egui; saneado; imágenes remotas bloqueadas; corpus de prueba; informe de fidelidad | ☑ |
| F0.4 | `model` + `store`: tipos del dominio; redb + blobs zstd + tantivy; benchmark 100 k mensajes | ☑ |
| F0.5 | `autoconfig` + `accounts`: ISPDB, autoconfig del dominio, Autodiscover, SRV/MX; llavero; sesión compartida Iurefficient; prueba de conexión IMAP/SMTP | ☑ |
| F0.6 | `engine` + `ui` + app: riel + 3 paneles, densidades, tema, paleta, i18n, asistente de alta, buzón de ejemplo, `ui_shot` | ☐ |
| F0.7 | IureProject migra a egui 0.37 + `iurefficient-ui` (rama `ui-kit` en su repo) | ☐ |

## F1 · Correo MVP

| Id | Tarea | Estado |
|---|---|---|
| F1.2 | `compose` (L3): borrador (`Draft`: identidad, Para/CC/CCO validados con IDNA, prioridad, acuse de lectura, envío programado, invitación iMIP); `RichBody` → HTML apto para correo (estilos en línea, sin tablas, enlaces/imágenes filtrados) y `text/plain; format=flowed`, importación HTML (html5ever, a lo mejor posible) y Markdown (pulldown-cmark) ida y vuelta; responder/responder a todos/reenviar (Re:/RE:/RV:/Fwd:, `Reply-To`, `Mail-Followup-To`, identidades propias, atribución es/en, reenvío en línea con adjuntos o como `message/rfc822`); MIME con mail-builder (alternative/related/mixed, RFC 2047/2231, Message-ID por dominio, Bcc fuera, `text/calendar` en alternative para Outlook, aviso > 25 MB); firmas por identidad y plantillas en el almacén; `Transport` + SMTP lettre (465/587, PLAIN/LOGIN/XOAUTH2); bandeja de salida sobre la cola (deshacer 0–30 s, programado, reintentos exponenciales, errores es/en, `mail.sync` → `AppendToSent`/marcar original). Pendiente: transportes Graph `sendMail` y Gmail API; partes locales no ASCII (SMTPUTF8); que `sync` consuma `mail.sync`; edición de las firmas en la UI | ◐ |

## F2 · OAuth + Calendario + Contactos

| Id | Tarea | Estado |
|---|---|---|
| F2.1 | `oauth`: código + PKCE con receptor loopback (RFC 8252), Google y Microsoft v2 (+ genérico), client id por compilación/entorno/ajustes, tokens (refresco en llavero, acceso en memoria, rotación, `invalid_grant` → volver a iniciar sesión), SASL XOAUTH2/OAUTHBEARER; servidor falso de pruebas; `docs/oauth.md` | ☑ |
| F2.2 | `dav` (L3): iCalendar/vCard sin pérdida (parser propio; `calcard` e `icalendar` evaluados), zonas IANA + Windows + `VTIMEZONE`, recurrencias con `rrule` y topes, iTIP/iMIP (REQUEST/REPLY/CANCEL/COUNTER, nuevo vs. actualización), cliente CalDAV/CardDAV (descubrimiento, colecciones, sync RFC 6578 con caída a ctag+ETag, `calendar-query`, `multiget`, `PUT`/`DELETE` con ETag, free/busy RFC 6638), servidor falso de pruebas, `dav_probe`. Pendiente: mapear a `model`; `RANGE=THISANDFUTURE`; `VTIMEZONE` propio con horario de verano sin nombre IANA (hoy desfase fijo); probar contra iCloud/Google/Fastmail reales y la instancia Iurefficient | ◐ |

## F3 · Iurefficient

| Id | Tarea | Estado |
|---|---|---|
| F3.0 | Inventario de la API de la instancia para el PIM y propuesta de lo que falta (REST + CalDAV/CardDAV, vincular correo) en `docs/iurefficient-api.md`; módulo `pim` en `iurefficient-connect` (rama `iuredesk`, sin publicar) | ◐ |
