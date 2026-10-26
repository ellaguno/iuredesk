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

## F3 · Iurefficient

| Id | Tarea | Estado |
|---|---|---|
| F3.0 | Inventario de la API de la instancia para el PIM y propuesta de lo que falta (REST + CalDAV/CardDAV, vincular correo) en `docs/iurefficient-api.md`; módulo `pim` en `iurefficient-connect` (rama `iuredesk`, sin publicar) | ◐ |
