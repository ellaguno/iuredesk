# Excepciones a la política "solo Rust"

PLAN.md D1. Cada excepción tiene su línea en `xtask/purity-allow.txt`. Solo las aprueba el dueño del proyecto.

| Crate | Plataforma | Por qué se permite | Revisado |
|---|---|---|---|
| `wayland-sys` | Linux | Abre `libwayland-client` del sistema en tiempo de ejecución (feature `dlopen`); Wayland es el servidor gráfico del SO. No compila C. | 2026-10-23 |
| `wayland-backend` | Linux | Su *shim* en C solo se compila con la feature `log`, que no se activa. | 2026-10-23 |
| `x11-dl` | Linux | Abre Xlib del sistema en tiempo de ejecución; X11 es el servidor gráfico del SO. `pkg-config` solo localiza la biblioteca. | 2026-10-23 |
| `rayon-core` | Todas | Declara `links` solo para impedir dos versiones en un mismo binario; es Rust puro. Llega por las dependencias de pruebas (`egui_kittest`). | 2026-10-23 |
| `wasm-bindgen-shared` | Web | Declara `links` solo para impedir dos versiones de wasm-bindgen; Rust puro. | 2026-10-23 |
| `stylo` | Todas | **Pendiente de aprobación del dueño (F0.3).** Motor CSS de Servo/Firefox que usa Blitz. Declara `links = "servo_style_crate"` solo para impedir dos copias; no enlaza nada nativo y `bindgen` solo se usa con la feature `gecko` (no activa). Ojo: su `build.rs` ejecuta **`python3`** (plantillas Mako → código Rust), así que compilar exige Python en la máquina; el binario resultante es Rust puro. Ver `docs/html-view.md`. | 2026-10-26 |

Pendiente al llegar a Windows y macOS (Fase 7): revisar `windows-sys`/`windows-targets` (Win32) y
`objc2` (Cocoa), que son bindings del SO.

## JavaScript y TypeScript

La política «solo Rust» se refiere al código de IureProject. Excepciones acotadas:

| Archivo | Por qué |
|---|---|
| `crates/web/demo/index.html` (bloque `<script type="module">`) | Cargar el módulo WebAssembly y cablear la página de demostración; no tiene lógica del Gantt |
| `crates/web/demo/pkg/*.js` | Generado por `wasm-bindgen`; no se escribe a mano ni se versiona |
| `integrations/iurefficient/IureGantt.tsx` | Entrega para el frontend de Iurefficient (React/TypeScript); se copia a ese repositorio |

## `unsafe` en el oráculo del laboratorio

`unsafe_code` está prohibido (`forbid`) en todo el workspace salvo en `lab/oracle`, que lo tiene en
`deny` para que **un solo archivo**, `lab/oracle/src/com.rs`, pueda permitirlo: la automatización
COM de Microsoft Project (`CoCreateInstance`, `IDispatch::Invoke`) solo existe como API `unsafe`.
Cada bloque lleva su justificación (`// SAFETY:`). El oráculo es una herramienta de pruebas que
corre en la VM del laboratorio; no se distribuye ni lo usa ningún crate de la aplicación.

## `ring` (2026-10-24, decidido por el dueño del proyecto)

**Qué:** proveedor criptográfico de `rustls`, que usa `reqwest` dentro de `iurefficient-connect` para
hablar HTTPS con una instancia de Iurefficient (F5.2). Trae primitivas criptográficas en C y
ensamblador (`links = "ring_core_…"`, compila con `cc`).

**Por qué:** TLS necesita un proveedor criptográfico y los maduros (`ring`, `aws-lc-rs`) tienen
código nativo; el único 100 % Rust (`rustls-rustcrypto`) es experimental y sin auditoría. Es el mismo
que ya usan IureDav, IureTranscribe y las demás apps del ecosistema a través de `iurefficient-connect`.
Se mantiene la decisión de no usar OpenSSL.

**Alcance:** solo entra por `iureproject-connect`; el resto del workspace sigue sin código nativo.
