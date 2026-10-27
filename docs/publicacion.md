# Publicación e instaladores

El código es privado. Por decisión del dueño, la CI y las versiones de **prueba** viven por ahora en
el mismo repositorio privado `ellaguno/iuredesk` (borradores de Release marcados como preliminares);
nada se publica. Licencia: FSL-1.1-ALv2.

## Paquetes

| Orden | Resultado en `dist/` |
|---|---|
| `cargo xtask dist [--tag vX.Y.Z]` | `iuredesk-<versión>-x86_64-linux.tar.gz` (binario, `LICENSE.md`, `NOTICE`, `README.txt`, `THIRD-PARTY-NOTICES.txt`) + `SHA256SUMS` |
| `cargo xtask package linux` | lo anterior + `iuredesk_<versión>_amd64.deb` + `iuredesk_<versión>_x86_64.AppImage` |
| `cargo xtask package windows` | `IureDesk_<versión>_x64-setup.exe` (NSIS, por usuario, sin UAC) |
| `cargo xtask package macos [--target x86_64-apple-darwin]` | `IureDesk.app` + `IureDesk_<versión>_<arq>.dmg` |

Tamaños de referencia (0.1.0-alpha.0, Linux x86_64, 2026-10-26): `.tar.gz` 20,3 MB, `.deb` 20,5 MB
(51,8 MB instalado), AppImage 19,5 MB.

Opciones comunes: `--tag vX.Y.Z` (falla si no coincide con la versión del workspace) y
`--target <triple>`. Cada orden compila con el perfil `dist` (release sin símbolos), genera los
avisos de terceros para ese destino y deja `SHA256SUMS` con todos los paquetes.

`package` usa **cargo-packager 0.11.8** por su CLI (herramienta de compilación, aceptada por el
criterio del dueño; nada de ella entra en el binario): `cargo install cargo-packager --version
0.11.8 --locked` (o `CARGO_PACKAGER=/ruta/al/binario`). La configuración la arma `xtask`
(`xtask/src/package.rs`) y queda en `target/packager/<triple>/packager.json`. cargo-packager descarga
en la primera ejecución sus propias herramientas (linuxdeploy/AppRun en Linux, NSIS en Windows).

### Integración con el escritorio
- **Linux**: `packaging/linux/iuredesk.desktop` (`MimeType=x-scheme-handler/mailto;text/calendar;
  text/vcard;text/x-vcard;message/rfc822;`, `Categories=Office;Email;Calendar;ContactManagement;`,
  acción «Mensaje nuevo»), metainfo AppStream `com.iurefficient.IureDesk.metainfo.xml` en
  `/usr/share/metainfo/`, iconos hicolor 16–512 + SVG escalable, documentos en
  `/usr/share/doc/iuredesk/`.
- **Windows**: el instalador crea los ProgID `IureDesk.eml/.ics/.vcf` y `IureDesk.mailto` y declara
  las capacidades en `RegisteredApplications` (`packaging/windows/default-apps.nsh`): IureDesk
  aparece en *Configuración → Aplicaciones → Aplicaciones predeterminadas*; Windows no permite que un
  instalador se imponga como predeterminado, lo elige el usuario. El desinstalador lo borra.
- **macOS**: `Info.plist` con `CFBundleURLTypes` (`mailto`) y `CFBundleDocumentTypes` (`.eml`,
  `.ics`, `.vcf`), identificador `com.iurefficient.IureDesk`, macOS 11 o posterior.

### Icono
`assets/icons/iuredesk.svg` (y `iuredesk-small.svg` para 16–32 px) son originales del proyecto.
`cargo xtask icons` los rasteriza en Rust puro (resvg) a `iuredesk-<n>.png` (16–1024),
`iuredesk.ico` y `iuredesk.icns`; los generados se versionan. Si cambias un SVG, vuelve a correrlo.

## CI (`.github/workflows/ci.yml`)
En cada push a `main` y en cada PR: `cargo xtask ci` en Ubuntu 22.04, y compilación release +
pruebas en `windows-latest` y `macos-latest`.

## Versión de prueba (`.github/workflows/release.yml`)
Al subir una etiqueta `v*`: compila en paralelo Linux x86_64, Windows x86_64, macOS arm64 y macOS
Intel con `cargo xtask package`, junta todo, calcula `SHA256SUMS` y crea un **borrador preliminar**
de Release en este mismo repositorio (con `GITHUB_TOKEN`; notas de `docs/notas/<etiqueta>.md` si
existe). Sin etiqueta (*Run workflow*) solo deja los paquetes como artefactos de la ejecución.

1. Subir `version` en `[workspace.package]` de `Cargo.toml` (y la `<release>` del metainfo) y pasar
   `cargo xtask ci`.
2. `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. Revisar el borrador en *Releases* y descargar/probar los instaladores.

## Lo que tiene que hacer el dueño

### Obligatorio: acceso a los repos de la suite
El workspace depende de `ellaguno/iurefficient-ui` (privado) e `ellaguno/iurefficient-connect`
(público) por git. Los flujos reescriben `https://github.com/` con un token y usan el git del sistema
(`CARGO_NET_GIT_FETCH_WITH_CLI=true`).
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
- Icono incrustado en el `.exe` de Windows y en la ventana (necesita `build.rs`/`with_icon` en
  `apps/iuredesk`); hoy el instalador y los accesos usan el icono del `.exe`.
- La app abre `mailto:`; falta que abra rutas `.eml`/`.ics`/`.vcf` (y `file://…` que pasa `%U`).
- Flatpak; `.msi` (WiX) si algún cliente lo exige; binario universal de macOS.
- Autoactualización vía el módulo `releases` de connect; repositorio público de descargas cuando el
  dueño lo decida.
