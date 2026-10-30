# Excepciones a la política "solo Rust"

PLAN.md D1. Cada excepción tiene su línea en `xtask/purity-allow.txt`. Solo las aprueba el dueño del proyecto.

| Crate | Plataforma | Por qué se permite | Revisado |
|---|---|---|---|
| `wayland-sys` | Linux | Abre `libwayland-client` del sistema en tiempo de ejecución (feature `dlopen`); Wayland es el servidor gráfico del SO. No compila C. | 2026-10-23 |
| `wayland-backend` | Linux | Su *shim* en C solo se compila con la feature `log`, que no se activa. | 2026-10-23 |
| `x11-dl` | Linux | Abre Xlib del sistema en tiempo de ejecución; X11 es el servidor gráfico del SO. `pkg-config` solo localiza la biblioteca. | 2026-10-23 |
| `rayon-core` | Todas | Declara `links` solo para impedir dos versiones en un mismo binario; es Rust puro. Llega por las dependencias de pruebas (`egui_kittest`). | 2026-10-23 |
| `wasm-bindgen-shared` | Web | Declara `links` solo para impedir dos versiones de wasm-bindgen; Rust puro. | 2026-10-23 |
| `prettyplease` | Todas | Declara `links = "prettyplease03"` solo para publicar su versión a otros *build scripts*; Rust puro, no compila C. Llega por `tantivy` → `bon` → `bon-macros` (macro de compilación, no va en el binario). Aprobada 2026-10-26 por el criterio del dueño: solo afecta a la compilación, no al producto. | 2026-10-26 |
| `objc-sys` | macOS | **Aprobada por el dueño 2026-10-29.** Declara `links = "objc_0_3"` y enlaza `libobjc`, el runtime de Objective-C que trae macOS: una biblioteca del SO, como Wayland/X11 en Linux. No compila C. Llega por `objc2` 0.5 de `eframe` → `accesskit_macos`; `objc2` 0.6 (de `iuredesk-desktop` e `iuredesk-macos-events`) ya no lo trae. `cargo xtask purity aarch64-apple-darwin` y `x86_64-apple-darwin` pasan con ella. | 2026-10-29 |
| `stylo` | Todas | **Aprobada por el dueño 2026-10-26.** Motor CSS de Servo/Firefox que usa Blitz. Declara `links = "servo_style_crate"` solo para impedir dos copias; no enlaza nada nativo y `bindgen` solo se usa con la feature `gecko` (no activa). Ojo: su `build.rs` ejecuta **`python3`** (plantillas Mako → código Rust), así que compilar exige Python en la máquina; el binario resultante es Rust puro. Ver `docs/html-view.md`. | 2026-10-26 |

Criterio del dueño (2026-10-26): las herramientas de **desarrollo/compilación** (Python, generadores, macros) son aceptables; el **producto final** no debe requerir nada fuera de Rust y del SO.

Pendiente al llegar a Windows y macOS (Fase 7): revisar `windows-sys`/`windows-targets` (Win32) y
`objc2` (Cocoa), que son bindings del SO. En macOS el único hallazgo de `cargo xtask purity`,
`objc-sys` (runtime `libobjc` del SO), quedó aprobado por el dueño el 2026-10-29 (tabla de arriba).

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

## `unsafe` acotado en `iuredesk-macos-events` (decisión del dueño 2026-10-29)

**Qué:** `unsafe_code` sigue en `forbid` en todo el workspace (`[workspace.lints]`) salvo en
`crates/macos-events` (`iuredesk-macos-events`, L3), que no hereda esos lints (un `forbid` no se
puede relajar) y los copia a mano con `unsafe_code = "deny"`, `unsafe_op_in_unsafe_fn = "deny"` y
`clippy::undocumented_unsafe_blocks = "deny"`. Dentro del crate, `src/lib.rs` es `forbid` fuera de
macOS y solo el módulo `src/macos.rs` (compilado únicamente en macOS) tiene `allow(unsafe_code)`.

Desde R1.5 (decisión del dueño 2026-10-29) la excepción cubre **eventos de Apple y clics en
notificaciones**.

**Por qué:** macOS no pasa por argumento lo que el usuario abre con la app, sino como Apple Events:
`kAEGetURL` (`mailto:`, `iuredesk://`, también los enlaces de la suite) y `kAEOpenDocuments`
(`.eml`/`.ics`/`.vcf` desde el Finder o «Abrir con»). winit 0.30 no los reenvía y registrar un
manejador en Cocoa exige pasar un objeto y un selector (`Sel`) que el compilador no puede comprobar.
Sin esto, en macOS esos enlaces y archivos traían la app pero no abrían nada (R1.3). Lo mismo con
el clic en un aviso: `UNUserNotificationCenter` solo lo cuenta a su delegado, un objeto que
implementa un protocolo de Objective-C (`define_class!`), y el destino del aviso viaja en su
`userInfo`, cuyo *setter* es `unsafe` porque el compilador no comprueba los tipos del diccionario.
Sin el delegado, el clic solo traía la app al frente (R1.2).

**Alcance exacto** (todo en `crates/macos-events/src/macos.rs`, cada bloque con `// SAFETY:`):
1. `define_class!` de `IureDeskAppleEventHandler` (subclase de `NSObject` sin variables ni `Drop`)
   con tres métodos: `handleGetURL:withReplyEvent:`, `handleOpenDocuments:withReplyEvent:` y
   `willFinishLaunching:`;
2. `msg_send![super(this), init]` al crearlo;
3. `NSAppleEventManager::setEventHandler_andSelector_forEventClass_andEventID` (`GURL`/`GURL` y
   `aevt`/`odoc`);
4. `NSNotificationCenter::addObserver_selector_name_object` para
   `NSApplicationWillFinishLaunchingNotification` (ahí se vuelven a registrar, por encima de los de
   AppKit, antes de que se despachen los eventos del arranque);
5. (R1.5) `define_class!` de `IureDeskNotificationDelegate` (subclase de `NSObject` sin variables
   ni `Drop`, de cualquier hilo: solo toca la bandeja) que implementa
   `UNUserNotificationCenterDelegate`: `willPresentNotification` (pide banner, lista y sonido para
   que el aviso salga con IureDesk al frente) y `didReceiveNotificationResponse` (salvo «descartar»,
   lee el enlace del `userInfo` y lo pasa a la bandeja; siempre llama al `completionHandler`), más
   su `msg_send![super(this), init]`. Se pone en `install()` (antes de que la app termine de
   arrancar, para recibir también el clic que la lanzó) y solo dentro del `.app`;
   `UNUserNotificationCenter.delegate` es débil, así que vive en un `thread_local` hasta el final
   del proceso;
6. (R1.5) `UNMutableNotificationContent::setUserInfo` con un `NSDictionary<NSString, NSString>`
   reinterpretado como `NSDictionary<AnyObject, AnyObject>` (`cast_unchecked`), en
   `set_notification_link`, que `iuredesk-desktop` llama al crear el aviso (así `desktop` sigue sin
   `unsafe`). Solo se guardan `iuredesk://message/<id>` e `iuredesk://event/<uid>` validados
   (`notification_link`, Rust seguro y probado) y lo leído se vuelve a validar.

El manejador vive lo que el proceso (no se libera nunca, así que no queda colgando). Lo recibido
pasa a una bandeja acotada en Rust seguro (64 elementos de ≤ 8 KiB, sin caracteres de control) que
la app vacía en cada cuadro. No se toca el delegado de winit. Dependencias: `objc2` 0.6 y
`objc2-foundation` 0.3 (bindings del SO en Rust; `objc2-core-services` solo aporta alias `u32`),
y desde R1.5 `objc2-user-notifications` 0.3 y `block2` 0.6 (ya los usaba `iuredesk-desktop`).

**Guarda:** `cargo xtask layers` (y `ci`) falla si otro crate no hereda `[workspace.lints]` o si
aparece la palabra clave `unsafe` en cualquier `.rs` del workspace fuera de
`crates/macos-events/src/macos.rs` (`xtask/src/unsafety.rs`).

## `ring` (2026-10-24, decidido por el dueño del proyecto)

**Qué:** proveedor criptográfico de `rustls`, que usa `reqwest` dentro de `iurefficient-connect` para
hablar HTTPS con una instancia de Iurefficient (F5.2). Trae primitivas criptográficas en C y
ensamblador (`links = "ring_core_…"`, compila con `cc`).

**Por qué:** TLS necesita un proveedor criptográfico y los maduros (`ring`, `aws-lc-rs`) tienen
código nativo; el único 100 % Rust (`rustls-rustcrypto`) es experimental y sin auditoría. Es el mismo
que ya usan IureDav, IureTranscribe y las demás apps del ecosistema a través de `iurefficient-connect`.
Se mantiene la decisión de no usar OpenSSL.

**Alcance:** entra por `iurefficient-connect` y, desde F0.5, por el TLS de correo y descubrimiento (`rustls`/`tokio-rustls` en `iuredesk-accounts`, `reqwest` en `iuredesk-autoconfig`, `lettre` para SMTP), siempre con el proveedor `ring` (nunca `aws-lc-rs` ni OpenSSL). El resto del workspace sigue sin código nativo. En Windows y macOS el llavero (`keyring` con `windows-native` / `apple-native`) trae `windows-sys` y `security-framework-sys`: bindings del SO, pendientes de la revisión de la Fase 7.
