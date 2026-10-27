# Importación (F5.1, crate `iuredesk-import`)

Migrar a IureDesk en minutos, todo en local. API: `Importer::new(&store).run(&[rutas], |evento| …)`
→ `ImportReport`; detección para el asistente: `detect(ruta)` y `find_installed()`. Prueba manual:

```sh
cargo run -p iuredesk-import --example import -- ~/.thunderbird --store /tmp/x
cargo run -p iuredesk-import --example import -- --detect
```

## Orígenes

| Origen | Qué se lee | Notas |
|---|---|---|
| Thunderbird | `profiles.ini` (Linux `~/.thunderbird`, snap, Flatpak; macOS `~/Library/Thunderbird`; Windows `%APPDATA%\Thunderbird`), `prefs.js` (cuentas, nombres, correo de la identidad, nombres de libretas), carpetas mbox (`X`, `X.sbd/`; `.msf` ignorado) o Maildir, `abook*.sqlite`/`history.sqlite` | `X-Mozilla-Status`/`Status2`/`Keys` → banderas y palabras clave; los expurgados sin compactar se omiten. `ImapMail` es una caché sin conexión: se importa y se avisa. Calendario local (`calendar-data/local.sqlite`): aviso para exportar a `.ics`. `.mab` (formato antiguo): aviso. RSS/NNTP: se ignoran con aviso. |
| Apple Mail | `~/Library/Mail/V<n>` más reciente; `Buzón.mbox/…/Messages/*.emlx`, paquetes anidados, `Buzón.mbox/mbox` exportado | Banderas y fecha del plist. `.partial.emlx`: se reinsertan los adjuntos de `Attachments/<id>/<parte>/`; si faltan, aviso. Contactos/Calendario: `.vcf`/`.ics` exportados. |
| Outlook | `.pst` Unicode y ANSI | Correo con carpetas, adjuntos, mensajes incrustados, banderas (leído, borrador, marcado, respondido, reenviado) y categorías; cabeceras de transporte originales si están; cuerpo HTML con su código de página, texto o RTF (LZFu). Contactos (correos, teléfonos, direcciones, foto), citas (zona de Windows → IANA, día completo, asistentes, recordatorio, repetición → `RRULE`/`EXDATE`), tareas, notas. Listas de distribución y diario: omitidos con motivo. Repeticiones movidas una a una: aviso. |
| Outlook `.ost` | — | El lector no admite los OST de Outlook 2013+: aviso con la alternativa (agregar la cuenta, o en Outlook *Archivo › Abrir y exportar › Importar o exportar* → `.pst`; o arrastrar correos a una carpeta para obtener `.eml`, y exportar el calendario como `.ics`). |
| Gmail Takeout | `Mail/*.mbox` con `X-Gmail-Labels`; `Calendar/*.ics`; `Contacts/*/*.vcf` | Etiquetas → carpeta (Papelera > Spam > Borradores > Recibidos > Enviados > primera etiqueta propia > Archivo) + palabras clave; `Unread`/`Starred` → banderas. Cuenta «Gmail (Takeout)». |
| Genéricos | `.mbox` (mboxo/mboxrd), Maildir y Maildir++ (`.Carpeta.Sub`), `.eml` sueltos o en carpetas (la ruta da la carpeta), `.emlx`, `.ics` (`VEVENT` → calendario, `VTODO` → tareas, `VJOURNAL` → notas), `.vcf` multi-tarjeta (vCard 2.1 *quoted-printable*, 3.0, 4.0; UTF-16 con BOM) | |

## Garantías

- **Por flujo y acotado:** mbox línea a línea (un archivo de decenas de GB o sin saltos de línea no
  se carga en memoria), `.ics`/`.vcf` por tandas; límites por mensaje (128 MiB), por objeto PIM
  (8 MiB), de profundidad y de número de archivos. Lo que los supera se omite con motivo.
- **Separadores `From_` hostiles:** separan solo al inicio o tras línea vacía y con forma de línea
  `From_` (hora y año); `>From` se des-escapa; CRLF y LF se conservan.
- **Reanudable:** `<almacén>/import/checkpoint.json` (unidades terminadas, byte alcanzado en cada
  mbox; la clave incluye tamaño y fecha, un archivo cambiado se relee). Se escribe tras confirmar
  cada lote; cancelar o un corte solo repiten el último lote.
- **Deduplicación:** `Message-ID` + huella del cuerpo normalizado (o, sin `Message-ID`, cabeceras
  estables + cuerpo), guardada como id remoto en la carpeta. En PST la clave sale de las
  propiedades (el MIME se regenera). Contactos, eventos y tareas por `UID`; notas por contenido.
- **Funciones de carpeta** por nombre en es/en/de/fr/it/pt/nl/sv («Bandeja de entrada»,
  «Elementos enviados», «Papelera», «Correo no deseado»…), solo en el primer nivel y una por
  cuenta; `[Gmail]/…` se aplana.
- Mensajes sin banderas en el origen se marcan leídos (`unknown_as_read`).
- El correo importado va a cuentas **locales** (sin sincronización), una por origen.

## Decisiones

- **PST:** `outlook-pst` 1.2 (Microsoft, MIT, Rust puro; depende de `byteorder`, `thiserror`,
  `tracing`). Lee bajo demanda. Algunas rutas hacen `expect`/indexan sobre datos del archivo: cada
  elemento se lee dentro de `catch_unwind` y un pánico cuenta como «elemento dañado»; las rutas que
  sabemos frágiles (`lookup_guid`, `lookup_string`) se sustituyen por lecturas propias acotadas.
  Las pruebas generan PST sintéticos con `outlook-pst-rw` (bifurcación MIT con escritor, solo
  `dev-dependency`).
- **SQLite de Thunderbird:** lector propio de solo lectura (`src/sqlite.rs`, ~500 líneas): árbol B de
  tablas, desbordamiento, registros, UTF-8/16 y WAL (tramas válidas hasta la última confirmación).
  Los lectores puros de crates.io eran jóvenes y con `unwrap` sobre datos del archivo; SQLite en C
  está prohibido. Libretas TB 102+ (`_vCard`) y campos clásicos. Las pruebas usan libretas
  sintéticas generadas con `tests/fixtures/make_sqlite.py` (herramienta de desarrollo).
- **Parseo MIME:** `iuredesk_sync::parse::envelope_from_raw` (el mismo de IMAP/POP3);
  iCalendar/vCard con `iuredesk-dav` + `iuredesk_pim_sync::map` (los de CalDAV/CardDAV). Las series
  con excepciones guardan su `.ics` como objeto crudo, así `occurrences_between` las expande sin
  pérdida.

## Pendiente

- Adjuntos de más de 128 MiB y mensajes gigantes: hoy se omiten (el almacén de blobs recibe el
  mensaje entero, sin flujo por partes).
- Excepciones de citas repetidas de Outlook (`ExceptionInfo`), listas de distribución, diario.
- OST modernos (bloques comprimidos de 4 KiB) y `.msg` sueltos.
- Calendario local de Thunderbird sin exportar (`local.sqlite`), libretas `.mab` (Mork).
- Comando del motor/UI (asistente de migración) que llame a `find_installed` + `Importer`.
