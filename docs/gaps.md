# Lista de trabajo

Ids `F<fase>.<n>`. Estado: ☐ pendiente · ◐ en curso · ☑ hecho.

## F0 · Cimientos

| Id | Tarea | Estado |
|---|---|---|
| F0.1 | Workspace, xtask (ci/layers/purity/licenses/notices/assets/dist), CLAUDE.md, licencia | ☑ |
| F0.2 | `iurefficient-ui`: extraer tema, tokens, iconos, prefs (tema del sistema, fuentes), a11y, paleta de comandos, lista virtual densa de IureProject; egui 0.37 | ◐ |
| F0.3 | `html-view`: prototipo Blitz → textura egui; saneado; imágenes remotas bloqueadas; corpus de prueba; informe de fidelidad | ◐ |
| F0.4 | `model` + `store`: tipos del dominio; redb + blobs zstd + tantivy; benchmark 100 k mensajes | ◐ |
| F0.5 | `autoconfig` + `accounts`: ISPDB, autoconfig del dominio, Autodiscover, SRV/MX; llavero; sesión compartida Iurefficient; prueba de conexión IMAP/SMTP | ☑ |
| F0.6 | `engine` + `ui` + app: riel + 3 paneles, densidades, tema, paleta, i18n, asistente de alta, buzón de ejemplo, `ui_shot` | ☐ |
| F0.7 | IureProject migra a egui 0.37 + `iurefficient-ui` (rama `ui-kit` en su repo) | ☐ |

## F2 · OAuth + Calendario + Contactos

| Id | Tarea | Estado |
|---|---|---|
| F2.1 | `oauth`: código + PKCE con receptor loopback (RFC 8252), Google y Microsoft v2 (+ genérico), client id por compilación/entorno/ajustes, tokens (refresco en llavero, acceso en memoria, rotación, `invalid_grant` → volver a iniciar sesión), SASL XOAUTH2/OAUTHBEARER; servidor falso de pruebas; `docs/oauth.md` | ☑ |
| F2.2 | `dav` (L3): iCalendar/vCard sin pérdida (parser propio; `calcard` e `icalendar` evaluados), zonas IANA + Windows + `VTIMEZONE`, recurrencias con `rrule` y topes, iTIP/iMIP (REQUEST/REPLY/CANCEL/COUNTER, nuevo vs. actualización), cliente CalDAV/CardDAV (descubrimiento, colecciones, sync RFC 6578 con caída a ctag+ETag, `calendar-query`, `multiget`, `PUT`/`DELETE` con ETag, free/busy RFC 6638), servidor falso de pruebas, `dav_probe`. Pendiente: mapear a `model`; `RANGE=THISANDFUTURE`; `VTIMEZONE` propio con horario de verano sin nombre IANA (hoy desfase fijo); probar contra iCloud/Google/Fastmail reales y la instancia Iurefficient | ◐ |

## F3 · Iurefficient

| Id | Tarea | Estado |
|---|---|---|
| F3.0 | Inventario de la API de la instancia para el PIM y propuesta de lo que falta (REST + CalDAV/CardDAV, vincular correo) en `docs/iurefficient-api.md`; módulo `pim` en `iurefficient-connect` (rama `iuredesk`, sin publicar) | ◐ |
