# Publicación e instaladores

Código abierto (Apache-2.0) y público desde 2026-10-28. La CI y los releases viven en el mismo
repositorio `ellaguno/iuredesk`; no hay repositorio aparte de descargas. Cada etiqueta publica un
Release normal (el «último»), que es lo que instala y actualiza
[iureapps](https://github.com/ellaguno/iureapps), el lanzador de la suite.

## Paquetes

| Orden | Resultado en `dist/` |
|---|---|
| `cargo xtask dist [--tag vX.Y.Z]` | `iuredesk-<versión>-x86_64-linux.tar.gz` (binario, `LICENSE.md`, `NOTICE`, `README.txt`, `THIRD-PARTY-NOTICES.txt`) + `SHA256SUMS` |
| `cargo xtask package linux` | lo anterior + `IureDesk_<versión>-linux-x64.deb` + `IureDesk_<versión>-linux-x64.AppImage` |
| `cargo xtask package windows` | `IureDesk_<versión>-windows-x64.exe` (NSIS, por usuario, sin UAC, silencioso con `/S`) |
| `cargo xtask package macos [--target x86_64-apple-darwin]` | `IureDesk.app` + `IureDesk_<versión>-macos-<arm64\|x64>.app.tar.gz` + `IureDesk_<versión>-macos-<arm64\|x64>.dmg` |
| `cargo xtask sign <archivos…>` | `<archivo>.sig` (minisign; ver «Firmas para iureapps») |

Los nombres siguen el esquema de iureapps (`catalog.rs`, `select_asset`): el instalador se reconoce
por el final `-<plataforma><ext>` con plataforma `linux-x64`, `windows-x64`, `macos-arm64` o
`macos-x64`, y su firma es el mismo nombre más `.sig`. No hay `.rpm`: cargo-packager 0.11.8 no lo
genera (en Fedora/openSUSE iureapps usa el AppImage). iureapps instala el AppImage en
`~/.local/share/iureapps/apps/IureDesk.AppImage` con su propia entrada de menú, el `.deb` (que deja
`/usr/bin/iuredesk`, el `linux_bins` del catálogo), el `.exe` con `/S` (clave `Uninstall\IureDesk`
de HKCU) y el `.app.tar.gz` en `/Applications/IureDesk.app`.

Tamaños de referencia (0.1.0-alpha.3, Linux x86_64, 2026-10-29): `.tar.gz` 29,7 MB, `.deb` 29,9 MB,
AppImage 28,0 MB (0.1.0-alpha.0: 20,3 / 20,5 / 19,5 MB). Revisados por dentro: `.desktop` válido
(`desktop-file-validate`), metainfo válido (`appstreamcli validate`), iconos hicolor 16–512 + SVG.

Opciones comunes: `--tag vX.Y.Z` (falla si no coincide con la versión del workspace) y
`--target <triple>`. Cada orden compila con el perfil `dist` (release sin símbolos), genera los
avisos de terceros para ese destino y deja `SHA256SUMS` con todos los paquetes.

`package` usa **cargo-packager 0.11.8** por su CLI (herramienta de compilación, aceptada por el
criterio del dueño; nada de ella entra en el binario): `cargo install cargo-packager --version
0.11.8 --locked` (o `CARGO_PACKAGER=/ruta/al/binario`). La configuración la arma `xtask`
(`xtask/src/package.rs`) y queda en `target/packager/<triple>/packager.json`. cargo-packager descarga
en la primera ejecución sus propias herramientas (linuxdeploy/AppRun en Linux, NSIS en Windows).

### Integración con el escritorio
El identificador es el mismo en todo: `com.iurefficient.IureDesk` (id de AppStream,
`CFBundleIdentifier`, AUMID de Windows y `APP_ID` de `iuredesk-desktop`; lo comprueba
`xtask package::tests::identifier_matches_app`). La app recibe por argumento rutas, `file://`,
`mailto:` e `iuredesk://…` (una segunda instancia se los pasa a la ventana abierta, R1.2).

- **Linux**: `packaging/linux/iuredesk.desktop` (`Exec=iuredesk %U`,
  `MimeType=x-scheme-handler/mailto;text/calendar;text/vcard;text/x-vcard;message/rfc822;x-scheme-handler/iuredesk;`,
  `Categories=Office;Email;Calendar;ContactManagement;`, acción «Mensaje nuevo»), metainfo AppStream
  `com.iurefficient.IureDesk.metainfo.xml` en `/usr/share/metainfo/`, iconos hicolor 16–512 + SVG
  escalable, documentos en `/usr/share/doc/iuredesk/`. El `.deb` y el AppImage llevan lo mismo
  (el AppImage trae el `.desktop` en su raíz para la integración de AppImageLauncher/iureapps).
- **Windows** (`packaging/windows/default-apps.nsh`, que xtask pasa como `preinstallSection` de la
  plantilla NSIS de cargo-packager; todo en HKCU, sin diálogos, así que `/S` sigue valiendo):
  - protocolo `iuredesk` (`HKCU\Software\Classes\iuredesk`, `URL Protocol`,
    `shell\open\command` = `"…\iuredesk.exe" "%1"`): el clic en un aviso lo usa;
  - AUMID `HKCU\Software\Classes\AppUserModelId\com.iurefficient.IureDesk` con `DisplayName` e
    `IconUri` (`iuredesk.ico` junto al `.exe`): los avisos salen aunque falte el acceso directo
    (la app también lo registra al arrancar). Los accesos directos del menú Inicio y del escritorio
    llevan el mismo AUMID por la plantilla de cargo-packager (`ApplicationID::Set`; el complemento
    lo descarga cargo-packager al compilar, como el propio NSIS);
  - ProgID `IureDesk.mailto` y `IureDesk.eml/.ics/.vcf` (orden con comillas), `OpenWithProgids`
    de cada extensión («Abrir con»), `Applications\iuredesk.exe` y las capacidades en
    `RegisteredApplications`: IureDesk aparece en *Configuración → Aplicaciones → Aplicaciones
    predeterminadas*. Windows no permite que un instalador se imponga como predeterminado; lo elige
    el usuario.
  - El desinstalador lo borra todo (el protocolo solo si apunta a esta instalación; las claves
    `.eml`/`.ics`/`.vcf` solo si quedan vacías).
  - No se usan `fileAssociations` ni `deepLinkProtocols` de cargo-packager 0.11.8 en Windows: su
    macro escribe la orden sin comillas en la ruta del `.exe`, se apropia del valor por omisión de
    `.eml` y su desinstalador no borra ni asociaciones ni protocolos.
- **macOS**: `packaging/macos/Info.plist` (va en `macos.infoPlistPath` y cargo-packager lo mezcla
  sobre el que genera, que pone nombre, ejecutable, versión, icono `.icns` y macOS 11 como mínimo):
  `CFBundleIdentifier`, `CFBundleURLTypes` (`iuredesk` y `mailto`), `CFBundleDocumentTypes`
  (`.eml` `com.apple.mail.email`, `.ics` `com.apple.ical.ics`, `.vcf` `public.vcard`; rol
  *Viewer*, rango *Alternate*) y `LSApplicationCategoryType` productividad. No hace falta
  `NSUserNotificationAlertStyle` (los avisos usan `UNUserNotificationCenter`).
  macOS entrega los enlaces y archivos como Apple Events, no por argumento, y winit 0.30 no los
  expone: los recoge `iuredesk-macos-events` (R1.4, único `unsafe` del workspace, ver
  `docs/excepciones.md`), que registra `kAEGetURL` y `kAEOpenDocuments` en `NSAppleEventManager`
  antes del bucle de eventos y los pasa por el mismo camino que los argumentos (también en frío:
  lo que llega antes de la ventana espera en una bandeja). El clic en un aviso del sistema lo
  recibe el delegado de `UNUserNotificationCenter` del mismo crate (R1.5): el aviso lleva su
  destino (`iuredesk://message/<id>` o `iuredesk://event/<uid>`) en el `userInfo` y entra en la
  misma bandeja; con la app al frente los avisos también se muestran. Solo se ha comprobado que compila para
  `aarch64-apple-darwin` y `x86_64-apple-darwin`; falta la prueba manual de abajo.

  **Prueba manual en un Mac (dueño)**, con el `.app` del `.dmg` instalado en `/Applications`:
  1. Con IureDesk **cerrado**, en Terminal: `open "mailto:ana@example.com?subject=Hola"` (o elige
     IureDesk en Mail → Ajustes → General → Lector de correo y haz clic en un `mailto:` de Safari).
     Debe abrir la app con el redactor ya lleno (arranque en frío).
  2. Con IureDesk **abierto**, repite con otra dirección: se abre otro redactor y la ventana pasa
     al frente (sin segunda instancia en el Dock).
  3. En el Finder, clic derecho en un `.eml` → Abrir con → IureDesk: sale la ventana de solo
     lectura del mensaje. Arrastra a la vez un `.ics` y un `.vcf` al icono del Dock: se abren la
     invitación/eventos y los contactos (prueba con la app cerrada y abierta).
  4. `open "iuredesk://compose?to=luis@example.com"` y, con un mensaje sincronizado,
     `open "iuredesk://message/<Message-ID>"`: abren el redactor y el mensaje.
  5. Avisos (permite las notificaciones de IureDesk la primera vez que lo pida, o en Ajustes del
     Sistema → Notificaciones): con una cuenta que reciba correo, mándate un mensaje y espera el
     aviso. Con IureDesk **al frente** el aviso también debe salir. Pon otra app al frente y haz
     clic en el aviso: IureDesk pasa al frente con ese mensaje abierto (su carpeta y pestaña).
     Repite con el recordatorio de un evento (abre el editor del evento) y, con IureDesk
     **cerrado** (Cmd+Q), haz clic en un aviso que siga en el Centro de notificaciones: debe abrir
     la app y el mensaje. Cerrar el aviso con la «x» no abre nada.
  6. Si algo no llega: `log stream --predicate 'process == "iuredesk"'` mientras repites el paso y
     anota qué pasó; no debe haber cierres ni diálogos.

Pruebas (`cargo test -p xtask package`): `windows_registration` expande los `!define` e
`!insertmacro` de `default-apps.nsh` y comprueba cada clave, que las órdenes llevan comillas, que no
toca HKLM/HKCR ni muestra diálogos y que el desinstalador borra cada valor que escribe;
`macos_info_plist` lee el `Info.plist` con el crate `plist` (el mismo que usa cargo-packager);
`desktop_entry` revisa el `.desktop`.

### Icono
`assets/icons/iuredesk.svg` (y `iuredesk-small.svg` para 16–32 px) son originales del proyecto.
`cargo xtask icons` los rasteriza en Rust puro (resvg) a `iuredesk-<n>.png` (16–1024),
`iuredesk.ico` y `iuredesk.icns`; los generados se versionan. Si cambias un SVG, vuelve a correrlo.

## CI (`.github/workflows/ci.yml`)
En cada push a `main` y en cada PR: `cargo xtask ci` en Ubuntu 22.04, y compilación release +
pruebas en `windows-latest` y `macos-latest`.

## Publicación (`.github/workflows/release.yml`)
Al subir una etiqueta `v*`: compila en paralelo Linux x86_64, Windows x86_64, macOS arm64 y macOS
Intel con `cargo xtask package`, firma cada paquete con `cargo xtask sign` (si existe el secreto
`IUREDESK_MINISIGN_KEY`; si no, avisa y sigue sin `.sig`), junta todo, escribe `latest.json` y
`SHA256SUMS` y **publica** el Release en este mismo repositorio (con `GITHUB_TOKEN`; notas de
`docs/notas/<etiqueta>.md` si existe). Sin etiqueta (*Run workflow*) solo deja los paquetes como
artefactos de la ejecución.

**Por qué no es borrador ni preliminar**: iureapps busca la versión con
`GET /repos/ellaguno/iuredesk/releases/latest`, y GitHub excluye de ahí los borradores y los
marcados como *prerelease*; con cualquiera de los dos iureapps diría «sin versiones publicadas». Por
eso el release sale normal y marcado como `--latest`; que es alfa lo dicen la versión
(`0.1.0-alpha.N`), el título «(alfa)» y el catálogo de iureapps (sección experimental, nota
«Versión alfa.»). `latest.json` (formato del actualizador de Tauri) es la alternativa que usa
iureapps cuando la API limita las consultas anónimas.

1. Subir `version` en `[workspace.package]` de `Cargo.toml` (y la `<release>` del metainfo) y pasar
   `cargo xtask ci`.
2. `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. Revisar el Release y probar la instalación desde iureapps (`iureapps instalar iuredesk`).

## Firmas para iureapps
iureapps verifica cada instalador con la firma minisign `<archivo>.sig` y la clave pública del
catálogo (`pubkey` de su `AppDef`), igual que el actualizador de Tauri: el `.sig` es el texto de la
firma minisign (precalculada, algoritmo `ED`) en base64, y la clave del catálogo es el texto de
`docs/minisign.pub` en base64. `cargo xtask sign` las produce con el crate `minisign` (Rust puro,
solo herramienta); la prueba `sign::tests::signature_verifies_like_iureapps` firma con una clave
nueva y verifica con el mismo algoritmo y crate que iureapps (`minisign-verify` 0.3).

- Clave pública: `docs/minisign.pub` (id `3B1C53D59E129491`), también en el catálogo de iureapps.
- Clave secreta: **solo** en `~/.config/iuredesk/minisign.key` del dueño (permisos 600, sin
  contraseña) y en el secreto de Actions. Nunca en un repositorio.
- Firmar a mano: `IUREDESK_MINISIGN_KEY_FILE=~/.config/iuredesk/minisign.key cargo xtask sign dist/IureDesk_*`
  (en CI la clave llega en `IUREDESK_MINISIGN_KEY`; si tuviera contraseña, en
  `IUREDESK_MINISIGN_PASSWORD`).
- Rotar la clave: `cargo xtask keygen <secreta> <pública>`, reemplazar `docs/minisign.pub`, la
  `pubkey` del catálogo de iureapps y el secreto.

## Lo que tiene que hacer el dueño

### Secreto de firma minisign (para iureapps)
Una vez, desde el equipo donde está la clave:

```sh
gh secret set IUREDESK_MINISIGN_KEY --repo ellaguno/iuredesk < ~/.config/iuredesk/minisign.key
```

La clave no tiene contraseña, así que no hace falta `IUREDESK_MINISIGN_PASSWORD`. Sin el secreto el
release sale sin `.sig` y iureapps instala avisando «se instala sin verificar».

### Opcional: acceso a repos privados de la suite
El workspace depende de `ellaguno/iurefficient-ui` e `ellaguno/iurefficient-connect`, ambos
**públicos**: no hace falta ningún token. Solo si alguno volviera a ser privado, los flujos
reescriben `https://github.com/` con `SUITE_REPOS_TOKEN` (git del sistema,
`CARGO_NET_GIT_FETCH_WITH_CLI=true`):
1. GitHub → *Settings → Developer settings → Fine-grained personal access tokens → Generate new
   token*. Propietario `ellaguno`; *Only select repositories*: `iurefficient-ui` (y
   `iurefficient-connect`, por si deja de ser público); permisos de repositorio: **Contents: Read-only**
   (Metadata: Read-only se agrega solo). Caducidad: la que prefieras (renuévalo antes).
2. En `ellaguno/iuredesk`: *Settings → Secrets and variables → Actions → New repository secret*
   `SUITE_REPOS_TOKEN` con ese token.
3. *Settings → Actions → General*: permitir acciones de GitHub y de terceros verificados (se usan
   `actions/*` y `Swatinem/rust-cache`). El job `release` pide `contents: write` por sí mismo.

### Opcional: firma en macOS (Developer ID + notarización)
Requiere el **Apple Developer Program** (99 USD/año).
1. Crear un certificado **Developer ID Application** (Xcode o developer.apple.com), exportarlo como
   `.p12` con contraseña.
2. Secretos: `APPLE_CERTIFICATE` (`base64 -i cert.p12`, una línea), `APPLE_CERTIFICATE_PASSWORD`,
   `APPLE_SIGNING_IDENTITY` (p. ej. `Developer ID Application: Eduardo Llaguno Velasco (TEAMID)`).
3. Notarización: `APPLE_ID` (correo de la cuenta), `APPLE_PASSWORD` (contraseña de app de
   appleid.apple.com, no la de la cuenta) y `APPLE_TEAM_ID`. (Alternativa: clave de App Store
   Connect con `APPLE_API_KEY`, `APPLE_API_ISSUER`, `APPLE_API_KEY_PATH`.)
Sin `APPLE_SIGNING_IDENTITY` el `.app`/`.dmg` sale sin firmar (Gatekeeper pedirá *clic derecho →
Abrir*).

### Opcional: firma en Windows
Una de dos:
- **Azure Trusted Signing** (recomendado; ~10 USD/mes, requiere verificación de identidad): crear la
  cuenta y el perfil de certificado en Azure, una *app registration* con el rol *Trusted Signing
  Certificate Profile Signer*, y en el repo los secretos `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`,
  `AZURE_TENANT_ID` y la **variable** `WINDOWS_SIGN_COMMAND` =
  `trusted-signing-cli -e https://<región>.codesigning.azure.net -a <cuenta> -c <perfil> -d IureDesk %1`
  (el flujo instala `trusted-signing-cli` si la variable empieza así).
- **Certificado de firma de código** (OV/EV de una CA) exportable como `.pfx`: secretos
  `WINDOWS_CERTIFICATE` (`.pfx` en base64), `WINDOWS_CERTIFICATE_PASSWORD` y
  `WINDOWS_CERTIFICATE_THUMBPRINT` (SHA-1 del certificado); variable opcional
  `WINDOWS_TIMESTAMP_URL` (por omisión `http://timestamp.digicert.com`). Los EV en token USB no
  sirven en la CI alojada.
Sin firma, SmartScreen avisará al instalar.

## Pendiente
- macOS: hacer la prueba manual de los Apple Events (ver «Integración con el escritorio»).
- Probar los instaladores de Windows y macOS en equipos reales (avisos, «Aplicaciones
  predeterminadas», desinstalación).
- Flatpak; `.msi` (WiX) si algún cliente lo exige; binario universal de macOS.
- Autoactualización dentro de la app vía el módulo `releases` de connect (hoy la actualiza iureapps).
- `.rpm` (cargo-packager no lo genera; haría falta otra herramienta).
