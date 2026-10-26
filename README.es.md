# IureDesk

**Gestor de información personal de escritorio, 100 % Rust** — correo, calendario, contactos,
notas y tareas — hecho para sustituir a Outlook, Thunderbird o Apple Mail a cualquier persona, con
más información en pantalla que cualquiera de ellos, y para ser la puerta de escritorio del
ecosistema Iurefficient. Linux primero; macOS y Windows después. [English version](README.md).

| Correo (claro, compacta) | Correo (oscuro, densa) |
|---|---|
| ![](docs/screens/mail-light.png) | ![](docs/screens/mail-dark-dense.png) |
| **Calendario** | **Paleta de comandos (Ctrl+K)** |
| ![](docs/screens/calendar-light.png) | ![](docs/screens/palette-light.png) |

Más capturas (todos los módulos, en claro y oscuro) en [`docs/screens/`](docs/screens).

## Qué es

- **Funciona sin Iurefficient.** Cualquier cuenta IMAP/SMTP, Gmail, Microsoft 365 o iCloud:
  escribes tu correo, IureDesk encuentra la configuración y muestra **un solo** botón.
- **Denso y tranquilo.** Tres densidades (la densa muestra más filas que Outlook), temas claro,
  oscuro y de alto contraste que siguen al escritorio en vivo, sin cinta: barra de ≤ 6 acciones y
  una paleta de comandos (`Ctrl+K`) que llega a todo.
- **Teclado primero, con las teclas que ya conoces**: atajos de Outlook, Gmail o Apple Mail.
- **Deshacer siempre**: archivar, eliminar, mover, banderas, tareas.
- **Offline-first**: todo se lee del almacén local (redb + tantivy); las acciones van a una cola
  local que la sincronización replica.
- **Privado**: el correo HTML se pinta con Blitz (sin webview ni JavaScript) y las imágenes
  remotas quedan bloqueadas hasta que las pides.
- **Rust puro**, sin bibliotecas de C ni webview; bilingüe (español/inglés).

## Pruébalo

```sh
cargo run -p iuredesk -- --sample      # buzón de ejemplo: 3 cuentas, ~1 900 mensajes, agenda, contactos…
cargo run -p iuredesk                  # tus cuentas (la primera vez abre el asistente de alta)
cargo run -p iuredesk -- mailto:ana@example.com?subject=Hola
```

Capturas sin pantalla: `cargo run -p iuredesk-ui --example ui_shot -- /tmp/shot.png --module calendar --dark`.
Puerta de calidad: `cargo xtask ci`. El plan está en [`PLAN.md`](PLAN.md) y la lista de trabajo en
[`docs/gaps.md`](docs/gaps.md).

## Licencia

[FSL-1.1-ALv2](LICENSE.md) (Functional Source License, que pasa a Apache-2.0 a los dos años),
© Eduardo Llaguno Velasco.
