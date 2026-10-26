# Render de correo HTML sin webview (F0.3)

Prototipo del riesgo nº 1 del proyecto (PLAN.md §2 D3, §12): ¿se puede mostrar correo HTML real con
fidelidad, en Rust puro, sin webview? Crate: `crates/html-view` (`iuredesk-html-view`, capa L5).

**Veredicto: GO con respaldo.** Blitz 0.3.0-beta.4 pinta bien la gran mayoría del corpus (boletines
con tablas, Outlook/Word, respuestas de Gmail, CSS moderno, SVG, CJK y emoji a color, RTL), es
seguro por construcción (no hay JavaScript) y funciona sin GPU. Quedan huecos concretos (abajo) que
se tapan con la hoja de estilo de correo propia, el texto plano de respaldo y contribuciones
*upstream*. El riesgo pasa de «alto» a «medio»: es una dependencia beta que se mueve rápido.

## Tubería

```
HTML hostil ─▶ sanitize (html5ever, por tokens) ─▶ Blitz: blitz-html + stylo + taffy + parley
           ─▶ blitz-paint ─▶ anyrender_vello_cpu (CPU, por franjas de 4096 px) ─▶ RGBA8 + enlaces
```

API (`src/lib.rs`):

- `render(html, &RenderOptions) -> Result<Rendered, Error>`.
  - `RenderOptions`: `viewport_width` (px CSS), `scale`, `dark_mode`, `allow_remote_images`,
    `inline_resources` (Content-ID → bytes), `fetcher: Option<Arc<dyn Fetcher>>`, `max_height`,
    `max_input_bytes`, `max_resource_bytes`.
  - `Rendered`: `width`, `height`, `rgba` (opaco), `links: Vec<LinkRect>` (px físicos, una zona por
    línea si el enlace se parte), `blocked` (URLs remotas bloqueadas, para la barra «Cargar
    imágenes»), `truncated`, `dark_applied`; `link_at(x, y)`.
- `sanitize(html) -> Sanitized` y `to_plain_text(html) -> String` (respaldo y base para citar).
- `warm_up()`: escanea las fuentes del sistema una vez (≈0,9–2 s en esta máquina); llamarlo al
  arrancar en un hilo de fondo.
- `Fetcher`: trait síncrono `fetch(url) -> Option<Bytes>`. El crate **nunca** abre conexiones; con
  `allow_remote_images = false` (por defecto) ni siquiera llama al `Fetcher`.

Ejemplo: `cargo run --release -p iuredesk-html-view --example render_corpus -- <dir> [--width 800]
[--scale 2] [--dark] [--only texto]`. Pinta cada correo a PNG e imprime tiempos.

## Seguridad

- Blitz no ejecuta JavaScript. Aun así el saneador (por tokens, con los mismos estados de texto
  crudo que el constructor de árbol) quita `<script>`, `<iframe>`, `<object>`, `<embed>`,
  `<applet>`, `<frameset>`, `<template>`, `<base>`, `<meta http-equiv>` (refresh), `<link>` que no
  sea hoja de estilo, atributos `on*`/`srcdoc`/`formaction`/`ping`, `action`/`method` de
  formularios, URLs `javascript:`/`vbscript:`/`file:`/`data:text/html` (también ofuscadas con
  entidades, tabuladores o mayúsculas), `expression()`/`-moz-binding` y, dentro de `<svg>`,
  cualquier `href` externo (usvg leería archivos locales). Los comentarios (incluidos los
  condicionales de Outlook) se eliminan, como hacen los clientes que no son Outlook.
- **Toda** petición de Blitz pasa por un `NetProvider` propio (`src/net.rs`): `cid:` →
  `inline_resources`; `data:` → decodificado local; `http(s)` → bloqueado y listado salvo permiso;
  todo lo demás (`file:`…) → nada. Siempre responde al instante (con bytes vacíos si no hay
  recurso), así una hoja «crítica» bloqueada no deja el documento sin pintar. Imágenes SVG con
  referencias externas se descartan. Límite de 2 000 peticiones por documento y de tamaño por
  recurso. Las URLs relativas se resuelven contra una base ficticia `x-iuredesk-mail://` que
  nadie descarga.
- Pruebas: 25 vectores XSS unitarios y una prueba de propiedades (512 casos) que retokeniza la salida
  del saneador y exige que no quede nada ejecutable. El render corre dentro de `catch_unwind`: un
  pánico de Blitz vuelve como `Error::EnginePanic` y la UI cae a texto plano. **Ya atrapó uno real**:
  Blitz entra en pánico con cualquier URL relativa si el documento no tiene base (corregido con la
  base ficticia; prueba de regresión `relative_urls_do_not_fail`).

## Corpus y fidelidad

`crates/html-view/tests/corpus/`: 26 correos sintéticos (escritos para la prueba, ninguno copiado) +
uno de 2 MB generado en `tests/common/mod.rs`. Lo que el dueño deje en
`tests/corpus/private/*.html` (ignorado por git) entra solo en la prueba y en el ejemplo (los `.eml`
llegarán cuando haya análisis MIME: `mail-parser` 1.0 exige rustc 1.98). Las imágenes `cid:` se
sintetizan según el Content-ID (`hero-600x220@x` → PNG de 600×220).

Revisé los PNG a mano. Funciona bien:

- Boletines con tablas anidadas, `bgcolor`, `width`, `<center>`, `align`, columnas «fantasma» de
  Outlook con `inline-block`, botones «a prueba de balas», imágenes `cid:` grandes escaladas,
  `background` en celdas, degradados, `border-radius`, sombras, `transform`, flexbox, grid,
  `@media` (ancho y `prefers-color-scheme`), `!important`, `@font-face` (bloqueado si es remoto).
- Respuestas de Gmail (`blockquote` anidados), respuestas de Outlook/Word (`MsoNormal`, `o:p`,
  tablas con bordes en pt), reenvíos con firmas y avatares redondos.
- SVG en línea (degradados, texto, rutas) y SVG adjunto por `cid:`.
- CJK (japonés, chino, coreano), emoji a color (Noto Color Emoji, CBDT) incluidos ZWJ y bandera
  arcoíris; árabe y hebreo con bidi mixto; tipografía (`sup`/`sub`, `mark`, versalitas, subrayado
  ondulado, `letter-spacing`, justificado).
- Modo oscuro: solo si el correo lo declara (`color-scheme` o `prefers-color-scheme`); si no, se
  respeta su diseño claro.
- HTML roto (etiquetas sin cerrar, mal anidadas, entidades sin `;`): mismo árbol que un navegador.

Huecos encontrados y qué se hizo:

1. **Atributos de presentación antiguos** que Blitz no traduce: `<table border>`, `cellpadding`,
   `cellspacing`, `valign`, `<table align=center>`, `<img align=left>`, `<br clear>`,
   `<font face/color/size>`, `background=`, `bordercolor`, `<ol type>`. Sin ellos los boletines
   antiguos salían sin bordes, sin centrar y con títulos ilegibles (texto negro sobre azul).
   **Resuelto** con una hoja de agente de usuario de correo (`src/ua.rs`, selectores de atributo;
   el CSS del correo siempre la vence) y estilo en línea saneado para los valores libres. Además
   Blitz compara `align` sensible a mayúsculas (`ALIGN=CENTER` no centraba): el saneador pasa
   `align`/`valign`/`clear` a minúsculas. Ambas cosas merecen PR *upstream*.
2. **Emoji a color invisibles**: `glifo` necesita su feature `png` para los glifos CBDT. Resuelto
   activándola. Persisten: banderas de país (indicadores regionales) salen como letras en caja.
3. **Listas en RTL**: las viñetas quedan a la izquierda en un documento `dir=rtl` (deberían ir a la
   derecha). Sin resolver (Blitz).
4. Menores: `-webkit-line-clamp` se ignora (el párrafo sale entero); el `alt` de imágenes rotas no
   se muestra (queda hueco); `width="33%"` en celdas se reparte desigual cuando las imágenes
   internas tienen `width=100%`; texto en `<title>` sin cerrar se come el documento (igual que un
   navegador). Las URL largas sin espacios dentro de celdas de ancho fijo desbordan (la hoja de
   correo pone `overflow-wrap: break-word`, que arregla párrafos pero no celdas, como en los
   navegadores); el búfer se ensancha hasta 2× el viewport en vez de cortar.

Los tres peores fallos que quedan: (a) viñetas en RTL del lado equivocado, (b) banderas como
letras, (c) `line-clamp`/`alt` ausentes. Ninguno impide leer el correo.

## Tiempos

Máquina de desarrollo **muy cargada** (load average ≈ 48 durante las mediciones, otros agentes
compilando), `--release`, un hilo, CPU. Los números varían ×2–×3 entre corridas:

| Caso | 800 px, escala 1 | escala 2 |
|---|---|---|
| Correos típicos (2–5 KB) | 3–60 ms (media ≈ 15–30 ms) | 50–350 ms |
| Imagen `cid:` de 1600×900 escalada | 130–460 ms (decodificar + escalar) | ≈ 520 ms |
| 2 MB (2 800 filas), cortado a 12 000 px | 1,1–3,9 s | ≈ 4,8 s |
| Escaneo de fuentes del sistema (una vez) | 0,9–2 s | — |

Conclusiones: el render debe ir siempre en el runtime de fondo (ya es la arquitectura) y la UI
muestra primero el texto plano o un esqueleto. Para correos enormes conviene pintar por franjas a
demanda (la API interna ya pinta por franjas con `viewport_scroll`; falta exponerla) y cachear el
`Rendered` por mensaje. El layout es incremental en Blitz: un cambio de ancho no exige reanalizar.

## Tamaño y dependencias

- Binario de ejemplo (release, `strip`): **16,9 MB** con todo el motor (stylo, taffy, parley,
  skrifa, harfrust, ICU con datos compilados, usvg, image, vello_cpu). El impacto en `iuredesk`
  será del orden de 14–16 MB; aceptable para una app de escritorio, pero vigilar.
- 287 crates en el árbol de `iuredesk-html-view` (Linux). Todo Rust; sin `-sys`.
- Blitz exige **rustc 1.91**; el crate declara `rust-version = "1.91"` (el workspace dice 1.88).
- Versión fijada exacta (`=0.3.0-beta.4`): es beta y rompe API entre betas.
- Compilación en limpio: varios minutos (stylo es grande).

## Pureza

- `cargo xtask purity`: **un hallazgo**, `stylo` declara `links = "servo_style_crate"`. Es solo
  para impedir dos copias del motor; no enlaza nada nativo y `bindgen` solo aplica con la feature
  `gecko` (inactiva). Añadido a `xtask/purity-allow.txt` y `docs/excepciones.md` **pendiente de
  aprobación del dueño**. Advertencia: su `build.rs` ejecuta **python3** (plantillas Mako → Rust),
  así que compilar exige Python instalado (el binario no lo usa).
- Fuentes: la feature `system-fonts` de Blitz arrastra `yeslogic-fontconfig-sys` (C). Se desactivó;
  `src/fonts.rs` escanea en Rust puro los directorios de fuentes (Linux: `/usr/share/fonts`,
  `/usr/local/share/fonts`, `~/.local/share/fonts`, `~/.fonts`; macOS y Windows previstos) con el
  escáner de fontique, y fija a mano las familias genéricas (`sans-serif` → Arial/Liberation/
  DejaVu…, `serif`, `monospace`, `emoji` → Noto Color Emoji…) y los respaldos por escritura (CJK,
  árabe, hebreo, tailandés, devanagari). `IUREDESK_FONT_DIRS` añade directorios: así se pueden
  empaquetar fuentes de respaldo con la app (recomendado: Noto Sans/Serif/Mono, Noto Sans CJK y
  Noto Color Emoji, todas OFL) para no depender de lo que tenga el sistema.
- `fontdb` (usado por usvg para texto en SVG) lee la configuración de fontconfig con un analizador
  Rust (`fontconfig-parser`), sin la biblioteca C.
- `cargo xtask layers`: ok (L5 no depende de egui/eframe/winit).

## GPU (para después)

Hoy se rasteriza en CPU con `vello_cpu` (sin GPU, sin sorpresas de drivers, funciona sin pantalla).
Alternativas con la misma API `anyrender`: `anyrender_vello` (vello sobre wgpu, el mismo wgpu de
egui) o `anyrender_vello_hybrid`. Con wgpu se podría pintar directamente a una textura de egui sin
copiar el búfer. Solo vale la pena si el desplazamiento de correos largos se nota lento.

## Respaldo (diseño)

1. `render` falla (`Err`, incluido pánico atrapado) o excede tiempo → la UI muestra
   `to_plain_text(html)` con enlaces clicables y citas `>` y un aviso «vista simplificada».
2. Tiempo límite: el render corre en una tarea de fondo; si no termina en ~2 s se muestra el texto
   plano y se reemplaza cuando llegue la imagen (o nunca, si se cancela).
3. Para mensajes > 8 MB de HTML: directamente texto plano.
4. Botón «Abrir en el navegador» (exporta el HTML saneado a un archivo temporal y lo abre con el
   navegador del sistema): salida de emergencia para el 1 % que Blitz no pinte bien.
5. Contribuir *upstream* los huecos de arriba (atributos de presentación, `align` en mayúsculas,
   RTL en listas, `line-clamp`, `alt`).

## Siguiente paso

Llenar `tests/corpus/private/` con 100–500 correos reales del dueño (exportados como `.html`),
correr `render_corpus` y revisar las PNG: el corpus sintético no sustituye al real (PLAN.md §12).
