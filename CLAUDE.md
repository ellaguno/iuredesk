# IureDesk — instrucciones para agentes

PIM de escritorio (correo, calendario, contactos, notas, tareas) **100 % en Rust** para Linux, macOS
y Windows, que compite por sí solo con Outlook / Thunderbird / Apple Mail y potencia el ecosistema
Iurefficient. El plan maestro está en [`PLAN.md`](PLAN.md); léelo antes de una tarea grande. La lista
de trabajo es [`docs/gaps.md`](docs/gaps.md).

## Solo Rust
Solo crates escritos en Rust. Prohibidos los `-sys` que envuelven C/C++ (OpenSSL, SQLite, GTK,
libdbus…), webviews y JS/TS. Las bibliotecas del SO (Wayland/X11, Vulkan, D-Bus, Win32) sí.
`cargo xtask purity` lo verifica; una excepción nueva va en `xtask/purity-allow.txt` y
`docs/excepciones.md`, y la decide el dueño del proyecto (si la necesitas, documenta el motivo y
sigue; no la escondas).
Criterio del dueño: en desarrollo/compilación se aceptan herramientas externas (p. ej. `python3` para
`stylo`); el **producto final** no puede requerir nada fuera de Rust y del SO.
Elecciones hechas: `rustls` (no OpenSSL), `zbus` (no libdbus), `redb` (no SQLite), `rfd` con portal
XDG (no GTK), Blitz para HTML (no webview).

## Nunca fallar
- Sin `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!` fuera de pruebas;
  `unsafe_code = "forbid"`. Cada crate de producción empieza con el `#![deny(...)]` de los demás.
- Errores como `Result` con el tipo de error del crate (`thiserror`). Lo no terminado devuelve error.
- Todo dato externo (correos, servidores, archivos, MCP) es hostil: `get()` en vez de `[i]`,
  aritmética saturada o comprobada, tamaños acotados, bucles con límite, tiempos de espera en red.
- Cada corrección de fallo llega con su prueba de regresión. Parsers con pruebas de propiedades.

## Arquitectura (PLAN.md §4)
| Capa | Crate | Función |
|---|---|---|
| L1 | `model` | Tipos del dominio, sin E/S |
| L2 | `store`, `autoconfig` | redb + blobs + tantivy; descubrimiento de servidores |
| L3 | `accounts` (y luego `sync`, `provider-*`) | Cuentas, llavero, protocolos |
| L4 | `engine` | `Session`, comandos, deshacer, i18n |
| L5 | `html-view` | Blitz → imagen RGBA + mapa de enlaces (sin egui) |
| L6 | `ui` | Interfaz egui (usa `iurefficient-ui`, el ui-kit compartido con IureProject) |
| L7 | `apps/iuredesk` | Binario |

- **Todo es un comando** (id, etiquetas en/es, atajo, `run`); menús, atajos, paleta, CLI y MCP
  despachan por id. Las llamadas programáticas nunca abren diálogos.
- Capas verificadas por `cargo xtask layers`; un crate nuevo se registra en `LAYERS`
  (`xtask/src/main.rs`) y en `members` de `Cargo.toml`. Nada por debajo de L6 depende de egui/eframe/winit/rfd (`html-view` entrega píxeles RGBA; la textura la sube `ui`).
- La UI nunca bloquea: la red y el disco viven en un runtime tokio de fondo; la UI lee *snapshots*.
- Offline-first: la UI lee del almacén local; las acciones van a una cola que `sync` replica.
- Dependencias nuevas: decláralas en `[workspace.dependencies]` y úsalas con `{ workspace = true }`.

## UX (no negociable)
"Cualquier usuario" + alta densidad. Sin ribbon; barra contextual ≤ 6 acciones + paleta `Ctrl+K`.
Mensajes de error en lenguaje humano. Deshacer siempre. Tres densidades. Claro/oscuro/sistema.
Accesible (AccessKit, teclado completo). Ver PLAN.md §6.

## Ecosistema
- Iurefficient se habla **solo** a través de `iurefficient-connect` (git, tag fijo). Lo que falte
  (agenda, contactos, tickets, notas por REST/CalDAV/CardDAV) se agrega en connect, no aquí.
- Iurefficient **no provee buzones**: el correo siempre viene de cuentas propias del usuario
  (IMAP/POP/SMTP, Gmail, Microsoft 365, iCloud) y se *vincula* a casos/leads/tickets.
- Apps hermanas (IureEditor, IureDav, IureTranscribe, IureVoz, IureProject, IureOCR) por frontera de
  proceso: deep links, MCP, socket de IureVoz. Se ofrecen solo si están instaladas.
- UI compartida: `../iurefficient-ui` (crate `iurefficient-ui`), egui **0.37** en toda la suite.

## Puerta de calidad
Antes de cada commit: `cargo xtask ci` (fmt, clippy -D warnings, pruebas, layers, purity, licenses,
notices, assets). Mensajes de commit con el id de tarea de `docs/gaps.md` (p. ej. `F0.3: …`).

## Ver la aplicación
- `cargo run -p iuredesk` (con `--sample` carga un buzón de ejemplo).
- Captura sin pantalla: `cargo run -p iuredesk-ui --example ui_shot -- /tmp/shot.png [--dark]`.
  Para trabajo de UI, mira la captura antes de dar algo por terminado.

## Licencia
FSL-1.1-ALv2, titular Eduardo Llaguno Velasco. Activos: nada de Microsoft/Apple/Google; iconos
dibujados en código o libres, cada archivo de `assets/` con su fila en `ATTRIBUTION.md`.

## Idioma
Código e identificadores en inglés; comentarios y documentación en español. Textos de interfaz
siempre en los dos idiomas (`lang::pick(en, es)`).
