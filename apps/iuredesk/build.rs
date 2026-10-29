//! R1.2: icono y datos de versión del `.exe` de Windows (lo que muestran el Explorador, la barra
//! de tareas y «Propiedades → Detalles»). Solo al compilar para Windows, con `winresource`, que
//! llama a la herramienta de recursos del SDK (`rc.exe` con MSVC, `windres` con GNU): es un paso
//! de compilación; el binario no necesita nada. Si la herramienta falta, se avisa y se sigue sin
//! icono (nunca falla la compilación).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icons/iuredesk.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/icons/iuredesk.ico")
        .set("ProductName", "IureDesk")
        .set("FileDescription", "IureDesk — correo, calendario, contactos, notas y tareas")
        .set("CompanyName", "Iurefficient")
        .set("LegalCopyright", "© Eduardo Llaguno Velasco · Apache-2.0")
        .set("OriginalFilename", "iuredesk.exe")
        .set("InternalName", "iuredesk");
    if let Err(e) = res.compile() {
        println!("cargo:warning=IureDesk: el .exe queda sin icono ni datos de versión ({e})");
    }
}
