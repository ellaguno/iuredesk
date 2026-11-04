//! Instalador `.msi` de Windows, en Rust puro con los crates `msi` y `cab` (MIT; herramientas del
//! xtask, nada entra en el producto): la base de datos de Windows Installer con sus tablas (esquema
//! de la documentación pública del SDK de Windows Installer) y los archivos en un gabinete MSZIP
//! guardado dentro del propio `.msi`. Adaptado del de IureProject.
//!
//! A diferencia del `.exe` (NSIS, por usuario), el `.msi` instala para todos los usuarios en
//! `Archivos de programa\IureDesk` (pide elevación; es lo que despliegan los administradores con
//! GPO o Intune). Registra lo mismo que `packaging/windows/default-apps.nsh`, en `HKLM`: el
//! protocolo `iuredesk:`, `mailto:` y `.eml`, `.ics`, `.vcf` como **opción** en «Aplicaciones
//! predeterminadas» y «Abrir con», y el AppUserModelID de los avisos (también en los accesos
//! directos, con `MsiShortcutProperty`). Accesos directos en el menú Inicio y en el escritorio
//! (`DESKTOPSHORTCUT=0` omite el segundo). Sin cuadros de diálogo: barra de progreso de Windows.
//! Una versión nueva reemplaza a la anterior (misma `UpgradeCode`); al desinstalar, Windows
//! Installer borra todo lo que escribió.

use std::io::{Cursor, Write};

use msi::{Column, Insert, Package, PackageType, Value};
use sha2::{Digest, Sha256};

/// Clave de la suite en `HKLM` con la versión, la carpeta y el `ProductCode` de lo instalado
/// (la lee iureapps, `detect_suite_msi`).
const SUITE_KEY: &str = "Software\\Iurefficient\\IureDesk";

/// «Aplicaciones predeterminadas» (`HKLM`): lo que la app *puede* abrir; el usuario elige.
const CAPS_KEY: &str = "Software\\Iurefficient\\IureDesk\\Capabilities";

/// Ejecutable principal: debe ser el primero de `files`.
const MAIN_EXE: &str = "iuredesk.exe";

/// Icono junto al ejecutable: `IconUri` del AppUserModelID (icono de los avisos).
const ICON_FILE: &str = "iuredesk.ico";

/// `PKEY_AppUserModel_ID` (propsys.h): el AUMID de un acceso directo.
const PKEY_AUMID: &str = "{9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, 5";

/// Nombre del gabinete dentro del `.msi` (`Media.Cabinet` = `#` + este nombre).
const CABINET: &str = "producto.cab";

/// Tipos de archivo: extensión y descripción del ProgID.
const FILE_TYPES: &[(&str, &str)] = &[("eml", "E-mail message"), ("ics", "iCalendar file"), ("vcf", "vCard contact")];

/// Un GUID estable derivado de un texto (los códigos de componente no deben cambiar entre
/// versiones; el de producto cambia con cada versión).
pub fn guid(seed: &str) -> String {
    let h = Sha256::digest(format!("iuredesk/msi/{seed}").as_bytes());
    let hex: String = h.iter().take(16).map(|b| format!("{b:02X}")).collect();
    let part = |a: usize, b: usize| hex.get(a..b).unwrap_or("0");
    format!("{{{}-{}-{}-{}-{}}}", part(0, 8), part(8, 12), part(12, 16), part(16, 20), part(20, 32))
}

/// `ProductVersion` de Windows Installer (`mayor.menor.compilación`, la compilación hasta 65535,
/// y solo esos tres cuentan al actualizar): la preliminar va en la compilación para que ordene
/// antes de la final. `0.1.0-alpha.9` → `0.1.9`; `-beta.N` → `0.1.(300+N)`; `-rc.N` →
/// `0.1.(600+N)`; `0.1.0` → `0.1.999`; `0.1.1` → `0.1.1999`.
pub fn product_version(v: &str) -> Result<String, String> {
    let (core, pre) = v.split_once('-').unwrap_or((v, ""));
    let nums: Vec<u32> =
        core.split('.').map(|p| p.parse::<u32>().map_err(|_| format!("versión «{v}»"))).collect::<Result<_, _>>()?;
    let (Some(&major), Some(&minor), Some(&patch)) = (nums.first(), nums.get(1), nums.get(2)) else {
        return Err(format!("versión «{v}»: se espera X.Y.Z"));
    };
    let stage = if pre.is_empty() {
        999
    } else {
        let (name, n) = pre.split_once('.').unwrap_or((pre, "0"));
        let n: u32 = n.parse().map_err(|_| format!("versión «{v}»"))?;
        let base = match name {
            "alpha" => 0,
            "beta" => 300,
            "rc" => 600,
            _ => return Err(format!("versión «{v}»: etapa «{name}» desconocida")),
        };
        if n >= 300 {
            return Err(format!("versión «{v}»: número de etapa demasiado grande"));
        }
        base + n
    };
    let build = patch.checked_mul(1000).and_then(|p| p.checked_add(stage)).filter(|b| *b <= 65_535);
    let build = build.ok_or_else(|| format!("versión «{v}»: no cabe en ProductVersion"))?;
    if major > 255 || minor > 255 {
        return Err(format!("versión «{v}»: no cabe en ProductVersion"));
    }
    Ok(format!("{major}.{minor}.{build}"))
}

/// Un archivo que instala el paquete.
pub struct MsiFile {
    /// Nombre largo en la carpeta de instalación.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Clave de una fila de `File`/`Component` (identificador de Windows Installer).
fn key(name: &str) -> String {
    name.replace('-', "_")
}

/// Arma el `.msi`. `files` va en orden; el primero debe ser `iuredesk.exe` (lleva los accesos
/// directos y el registro) y entre ellos debe estar `iuredesk.ico`.
pub fn build(version: &str, identifier: &str, files: &[MsiFile], icon_ico: &[u8]) -> Result<Vec<u8>, String> {
    let io = |e: std::io::Error| format!("msi: {e}");
    let pv = product_version(version)?;
    let Some(main) = files.first().filter(|f| f.name == MAIN_EXE) else {
        return Err(format!("msi: el primer archivo debe ser {MAIN_EXE}"));
    };
    if !files.iter().any(|f| f.name == ICON_FILE) {
        return Err(format!("msi: falta {ICON_FILE}"));
    }
    let main_key = key(&main.name);
    // El código del paquete cambia con el contenido; el de producto, con la versión.
    let mut content = Sha256::new();
    for f in files {
        content.update(f.name.as_bytes());
        content.update(&f.bytes);
    }
    let content_hex: String = content.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect();
    let product_code = guid(&format!("product/{version}"));
    let upgrade_code = guid("upgrade");

    let mut pkg = Package::create(PackageType::Installer, Cursor::new(Vec::new())).map_err(io)?;
    pkg.set_database_codepage(msi::CodePage::Windows1252);
    {
        let s = pkg.summary_info_mut();
        s.set_codepage(msi::CodePage::Windows1252);
        s.set_title("Installation Database");
        s.set_subject("IureDesk");
        s.set_author("Eduardo Llaguno Velasco");
        s.set_comments(format!("IureDesk {version}: correo, calendario, contactos, notas y tareas"));
        s.set_keywords(&["Installer".to_string(), "IureDesk".to_string()]);
        s.set_creating_application("cargo xtask package windows");
        s.set_arch("x64");
        s.set_languages(&[msi::LanguageId::from_tag("en-US")]);
        s.set_uuid(uuid_of(&guid(&format!("package/{version}/{content_hex}")))?);
        // 500: Windows Installer 5.0 (Windows 7+), por `MsiShortcutProperty`. 2: archivos
        // comprimidos y nombres largos.
        s.set_page_count(500);
        s.set_word_count(2);
        s.set_doc_security(2);
    }

    let id = |n: &str, len: usize| Column::build(n).primary_key().id_string(len);
    let s = |n: &str, len: usize| Column::build(n).string(len);
    let sn = |n: &str, len: usize| Column::build(n).nullable().string(len);
    let txt = |n: &str, len: usize| Column::build(n).localizable().string(len);
    let txtn = |n: &str, len: usize| Column::build(n).localizable().nullable().string(len);
    let i16c = |n: &str| Column::build(n).int16();
    let i16n = |n: &str| Column::build(n).nullable().int16();
    let i32c = |n: &str| Column::build(n).int32();
    let table = |pkg: &mut Package<Cursor<Vec<u8>>>, name: &str, cols: Vec<Column>| {
        pkg.create_table(name, cols).map_err(|e| format!("msi: tabla {name}: {e}"))
    };
    let insert = |pkg: &mut Package<Cursor<Vec<u8>>>, name: &str, rows: Vec<Vec<Value>>| {
        pkg.insert_rows(Insert::into(name).rows(rows)).map_err(|e| format!("msi: filas de {name}: {e}"))
    };
    let st = |v: &str| Value::Str(v.to_string());
    let int = |v: i32| Value::Int(v);

    table(&mut pkg, "Property", vec![id("Property", 72), txt("Value", 0)])?;
    let props = [
        ("ProductCode", product_code.as_str()),
        ("UpgradeCode", upgrade_code.as_str()),
        ("ProductName", "IureDesk"),
        ("ProductVersion", pv.as_str()),
        ("ProductLanguage", "1033"),
        ("Manufacturer", "Iurefficient"),
        ("ALLUSERS", "1"),
        ("ARPPRODUCTICON", "iuredesk.ico"),
        ("ARPHELPLINK", "https://github.com/ellaguno/iuredesk"),
        ("ARPURLINFOABOUT", "https://iurefficient.com/"),
        ("ARPNOMODIFY", "1"),
        ("ARPNOREPAIR", "1"),
        ("SecureCustomProperties", "OLDERVERSIONFOUND;DESKTOPSHORTCUT"),
        // `msiexec /i … DESKTOPSHORTCUT=0` instala sin el acceso directo del escritorio.
        ("DESKTOPSHORTCUT", "1"),
    ];
    insert(&mut pkg, "Property", props.iter().map(|(k, v)| vec![st(k), st(v)]).collect())?;

    table(&mut pkg, "Directory", vec![id("Directory", 72), sn("Directory_Parent", 72), txt("DefaultDir", 255)])?;
    insert(
        &mut pkg,
        "Directory",
        vec![
            vec![st("TARGETDIR"), Value::Null, st("SourceDir")],
            vec![st("ProgramFiles64Folder"), st("TARGETDIR"), st(".")],
            vec![st("INSTALLDIR"), st("ProgramFiles64Folder"), st("IUREDE~1|IureDesk")],
            vec![st("ProgramMenuFolder"), st("TARGETDIR"), st(".")],
            vec![st("DesktopFolder"), st("TARGETDIR"), st(".")],
        ],
    )?;

    // Un componente por archivo, de 64 bits (256), con el archivo como clave.
    table(
        &mut pkg,
        "Component",
        vec![
            id("Component", 72),
            sn("ComponentId", 38),
            s("Directory_", 72),
            i16c("Attributes"),
            sn("Condition", 255),
            sn("KeyPath", 72),
        ],
    )?;
    table(
        &mut pkg,
        "File",
        vec![
            id("File", 72),
            s("Component_", 72),
            txt("FileName", 255),
            i32c("FileSize"),
            sn("Version", 72),
            sn("Language", 20),
            i16n("Attributes"),
            i32c("Sequence"),
        ],
    )?;
    let mut components = Vec::new();
    let mut file_rows = Vec::new();
    for (k, f) in files.iter().enumerate() {
        let fk = key(&f.name);
        components.push(vec![
            st(&fk),
            st(&guid(&format!("component/{}", f.name))),
            st("INSTALLDIR"),
            int(256),
            Value::Null,
            st(&fk),
        ]);
        let ext: String = f.name.rsplit_once('.').map(|(_, e)| e).unwrap_or("").chars().take(3).collect();
        let short = if ext.is_empty() { format!("FILE{k}") } else { format!("FILE{k}.{}", ext.to_uppercase()) };
        let size = i32::try_from(f.bytes.len()).map_err(|_| format!("msi: {} es demasiado grande", f.name))?;
        let seq = i32::try_from(k + 1).map_err(|_| "msi: demasiados archivos".to_string())?;
        // 512 = msidbFileAttributesVital.
        file_rows.push(vec![
            st(&fk),
            st(&fk),
            st(&format!("{short}|{}", f.name)),
            int(size),
            Value::Null,
            Value::Null,
            int(512),
            int(seq),
        ]);
    }
    // El acceso directo del escritorio va en su propio componente, con condición; su clave es un
    // valor del registro (HKLM = 2), porque un acceso directo no puede serlo.
    components.push(vec![
        st("AccesoEscritorio"),
        st(&guid("component/desktop-shortcut")),
        st("DesktopFolder"),
        int(256 + 4),
        st("DESKTOPSHORTCUT<>\"0\""),
        st("ClaveEscritorio"),
    ]);
    insert(&mut pkg, "Component", components)?;
    insert(&mut pkg, "File", file_rows)?;

    table(
        &mut pkg,
        "Feature",
        vec![
            id("Feature", 38),
            sn("Feature_Parent", 38),
            txtn("Title", 64),
            txtn("Description", 255),
            i16n("Display"),
            i16c("Level"),
            sn("Directory_", 72),
            i16c("Attributes"),
        ],
    )?;
    insert(
        &mut pkg,
        "Feature",
        vec![vec![
            st("Principal"),
            Value::Null,
            st("IureDesk"),
            st("Mail, calendar, contacts, notes and tasks"),
            int(1),
            int(1),
            st("INSTALLDIR"),
            int(0),
        ]],
    )?;
    table(&mut pkg, "FeatureComponents", vec![id("Feature_", 38), id("Component_", 72)])?;
    let mut feature_components: Vec<Vec<Value>> =
        files.iter().map(|f| vec![st("Principal"), st(&key(&f.name))]).collect();
    feature_components.push(vec![st("Principal"), st("AccesoEscritorio")]);
    insert(&mut pkg, "FeatureComponents", feature_components)?;

    table(
        &mut pkg,
        "Media",
        vec![
            Column::build("DiskId").primary_key().int16(),
            i32c("LastSequence"),
            txtn("DiskPrompt", 64),
            sn("Cabinet", 255),
            sn("VolumeLabel", 32),
            sn("Source", 72),
        ],
    )?;
    let last = i32::try_from(files.len()).map_err(|_| "msi: demasiados archivos".to_string())?;
    insert(
        &mut pkg,
        "Media",
        vec![vec![int(1), int(last), Value::Null, st(&format!("#{CABINET}")), Value::Null, Value::Null]],
    )?;

    table(
        &mut pkg,
        "Shortcut",
        vec![
            id("Shortcut", 72),
            s("Directory_", 72),
            txt("Name", 128),
            s("Component_", 72),
            s("Target", 72),
            sn("Arguments", 255),
            txtn("Description", 255),
            i16n("Hotkey"),
            sn("Icon_", 72),
            i16n("IconIndex"),
            i16n("ShowCmd"),
            sn("WkDir", 72),
        ],
    )?;
    let shortcut = |id: &str, dir: &str, component: &str| {
        vec![
            st(id),
            st(dir),
            st("IUREDE~1|IureDesk"),
            st(component),
            st(&format!("[#{main_key}]")),
            Value::Null,
            st("Mail, calendar, contacts, notes and tasks"),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            st("INSTALLDIR"),
        ]
    };
    insert(
        &mut pkg,
        "Shortcut",
        vec![
            shortcut("MenuInicio", "ProgramMenuFolder", &main_key),
            shortcut("Escritorio", "DesktopFolder", "AccesoEscritorio"),
        ],
    )?;
    // El AUMID en los accesos directos (lo mismo que `ApplicationID::Set` en el `.exe`).
    table(
        &mut pkg,
        "MsiShortcutProperty",
        vec![id("MsiShortcutProperty", 72), s("Shortcut_", 72), s("PropertyKey", 0), s("PropVariantValue", 0)],
    )?;
    insert(
        &mut pkg,
        "MsiShortcutProperty",
        vec![
            vec![st("AumidMenuInicio"), st("MenuInicio"), st(PKEY_AUMID), st(identifier)],
            vec![st("AumidEscritorio"), st("Escritorio"), st(PKEY_AUMID), st(identifier)],
        ],
    )?;

    table(
        &mut pkg,
        "Registry",
        vec![
            id("Registry", 72),
            i16c("Root"),
            txt("Key", 255),
            txtn("Name", 255),
            txtn("Value", 0),
            s("Component_", 72),
        ],
    )?;
    insert(&mut pkg, "Registry", registry_rows(version, identifier, &main_key))?;

    table(&mut pkg, "Icon", vec![id("Name", 72), Column::build("Data").binary()])?;
    insert(&mut pkg, "Icon", vec![vec![st("iuredesk.ico"), Value::Binary]])?;
    write_stream(&mut pkg, "Icon.iuredesk.ico", icon_ico)?;

    // Una versión anterior (de 0.0.0 a esta, sin incluirla) se desinstala antes de instalar.
    table(
        &mut pkg,
        "Upgrade",
        vec![
            Column::build("UpgradeCode").primary_key().string(38),
            Column::build("VersionMin").primary_key().nullable().string(20),
            Column::build("VersionMax").primary_key().nullable().string(20),
            Column::build("Language").primary_key().nullable().string(255),
            Column::build("Attributes").primary_key().int32(),
            sn("Remove", 255),
            s("ActionProperty", 72),
        ],
    )?;
    insert(
        &mut pkg,
        "Upgrade",
        vec![vec![
            st(&upgrade_code),
            st("0.0.0"),
            st(&pv),
            Value::Null,
            int(256),
            Value::Null,
            st("OLDERVERSIONFOUND"),
        ]],
    )?;

    let sequence = |pkg: &mut Package<Cursor<Vec<u8>>>, name: &str, actions: &[(&str, i32)]| -> Result<(), String> {
        table(
            pkg,
            name,
            vec![
                id("Action", 72),
                Column::build("Condition").nullable().string(255),
                Column::build("Sequence").nullable().int16(),
            ],
        )?;
        insert(
            pkg,
            name,
            actions.iter().map(|(a, n)| vec![Value::Str((*a).to_string()), Value::Null, Value::Int(*n)]).collect(),
        )
    };
    sequence(
        &mut pkg,
        "InstallExecuteSequence",
        &[
            ("FindRelatedProducts", 25),
            ("CostInitialize", 800),
            ("FileCost", 900),
            ("CostFinalize", 1000),
            ("InstallValidate", 1400),
            ("InstallInitialize", 1500),
            ("RemoveExistingProducts", 1501),
            ("ProcessComponents", 1600),
            ("UnpublishFeatures", 1800),
            ("RemoveRegistryValues", 2600),
            ("RemoveShortcuts", 3200),
            ("RemoveFiles", 3500),
            ("InstallFiles", 4000),
            ("CreateShortcuts", 4500),
            ("WriteRegistryValues", 5000),
            ("RegisterUser", 6000),
            ("RegisterProduct", 6100),
            ("PublishFeatures", 6300),
            ("PublishProduct", 6400),
            ("InstallFinalize", 6600),
        ],
    )?;
    sequence(
        &mut pkg,
        "InstallUISequence",
        &[
            ("FindRelatedProducts", 25),
            ("CostInitialize", 800),
            ("FileCost", 900),
            ("CostFinalize", 1000),
            ("ExecuteAction", 1300),
        ],
    )?;

    write_stream(&mut pkg, CABINET, &cabinet(files)?)?;
    pkg.flush().map_err(io)?;
    Ok(pkg.into_inner().map_err(io)?.into_inner())
}

/// Filas de `Registry`: lo de `default-apps.nsh`, por máquina. Raíz 0 = `HKCR` (con `ALLUSERS=1`,
/// `HKLM\Software\Classes`), 2 = `HKLM`. Un valor con nombre y `Value` nulo es una cadena vacía
/// (`"URL Protocol"`, `OpenWithProgids`). Todo cuelga del componente del ejecutable: se borra al
/// desinstalar.
fn registry_rows(version: &str, identifier: &str, main_key: &str) -> Vec<Vec<Value>> {
    let open = format!("\"[#{main_key}]\" \"%1\"");
    let icon = format!("\"[#{main_key}]\",0");
    let aumid = format!("AppUserModelId\\{identifier}");
    let mut rows: Vec<(String, i32, String, Option<String>, Option<String>)> = Vec::new();
    let mut add = |id: &str, root: i32, key: &str, name: Option<&str>, value: Option<&str>| {
        rows.push((id.to_string(), root, key.to_string(), name.map(str::to_string), value.map(str::to_string)));
    };
    // ProgID que abre un archivo o enlace con `"…\iuredesk.exe" "%1"`.
    let prog_id =
        |add: &mut dyn FnMut(&str, i32, &str, Option<&str>, Option<&str>), tag: &str, prog: &str, desc: &str| {
            add(&format!("{tag}Tipo"), 0, prog, None, Some(desc));
            add(&format!("{tag}Aumid"), 0, prog, Some("AppUserModelID"), Some(identifier));
            add(&format!("{tag}Icono"), 0, &format!("{prog}\\DefaultIcon"), None, Some(&icon));
            add(&format!("{tag}Abrir"), 0, &format!("{prog}\\shell\\open\\command"), None, Some(&open));
        };

    // AppUserModelID: nombre e icono de los avisos de una app sin paquete.
    add("AumidNombre", 0, &aumid, Some("DisplayName"), Some("IureDesk"));
    add("AumidIcono", 0, &aumid, Some("IconUri"), Some(&format!("[#{}]", key(ICON_FILE))));

    // Protocolo iuredesk: (clic en una notificación = activación por protocolo).
    add("ProtoNombre", 0, "iuredesk", None, Some("URL:IureDesk"));
    add("ProtoUrl", 0, "iuredesk", Some("URL Protocol"), None);
    add("ProtoIcono", 0, "iuredesk\\DefaultIcon", None, Some(&icon));
    add("ProtoAbrir", 0, "iuredesk\\shell\\open\\command", None, Some(&open));

    // mailto: con su propio ProgID (no se pisa HKCR\mailto de otra aplicación).
    prog_id(&mut add, "Mailto", "IureDesk.mailto", "URL:MailTo Protocol");
    add("MailtoUrl", 0, "IureDesk.mailto", Some("URL Protocol"), None);

    // La app en «Abrir con».
    let app = format!("Applications\\{MAIN_EXE}");
    add("AppNombre", 0, &app, Some("FriendlyAppName"), Some("IureDesk"));
    add("AppIcono", 0, &format!("{app}\\DefaultIcon"), None, Some(&icon));
    add("AppAbrir", 0, &format!("{app}\\shell\\open\\command"), None, Some(&open));

    for (ext, desc) in FILE_TYPES {
        let tag = format!("Ext{ext}");
        let prog = format!("IureDesk.{ext}");
        prog_id(&mut add, &tag, &prog, desc);
        add(&format!("{tag}Con"), 0, &format!(".{ext}\\OpenWithProgids"), Some(&prog), None);
        add(&format!("{tag}Soporta"), 0, &format!("{app}\\SupportedTypes"), Some(&format!(".{ext}")), None);
        add(
            &format!("{tag}Capacidad"),
            2,
            &format!("{CAPS_KEY}\\FileAssociations"),
            Some(&format!(".{ext}")),
            Some(&prog),
        );
    }

    // Capacidades para «Aplicaciones predeterminadas».
    add("CapNombre", 2, CAPS_KEY, Some("ApplicationName"), Some("IureDesk"));
    add(
        "CapDescripcion",
        2,
        CAPS_KEY,
        Some("ApplicationDescription"),
        Some("Mail, calendar, contacts, notes and tasks"),
    );
    add("CapIcono", 2, CAPS_KEY, Some("ApplicationIcon"), Some(&icon));
    add("CapMailto", 2, &format!("{CAPS_KEY}\\URLAssociations"), Some("mailto"), Some("IureDesk.mailto"));
    add("Registrada", 2, "Software\\RegisteredApplications", Some("IureDesk"), Some(CAPS_KEY));

    // Para iureapps (y la suite): la versión tal cual, la carpeta y el código para desinstalar.
    // Windows muestra `ProductVersion`, que no es la versión.
    add("SuiteVersion", 2, SUITE_KEY, Some("Version"), Some(version));
    add("SuiteCarpeta", 2, SUITE_KEY, Some("InstallDir"), Some("[INSTALLDIR]"));
    add("SuiteProducto", 2, SUITE_KEY, Some("ProductCode"), Some("[ProductCode]"));

    let opt = |v: &Option<String>| v.as_deref().map_or(Value::Null, |s| Value::Str(s.to_string()));
    let mut out: Vec<Vec<Value>> = rows
        .iter()
        .map(|(id, root, k, name, value)| {
            vec![
                Value::Str(id.clone()),
                Value::Int(*root),
                Value::Str(k.clone()),
                opt(name),
                opt(value),
                Value::Str(main_key.to_string()),
            ]
        })
        .collect();
    // Clave del componente del acceso directo del escritorio.
    out.push(vec![
        Value::Str("ClaveEscritorio".into()),
        Value::Int(2),
        Value::Str(SUITE_KEY.into()),
        Value::Str("DesktopShortcut".into()),
        Value::Str("#1".into()),
        Value::Str("AccesoEscritorio".into()),
    ]);
    out
}

fn write_stream(pkg: &mut Package<Cursor<Vec<u8>>>, name: &str, bytes: &[u8]) -> Result<(), String> {
    let mut w = pkg.write_stream(name).map_err(|e| format!("msi: flujo {name}: {e}"))?;
    w.write_all(bytes).map_err(|e| format!("msi: flujo {name}: {e}"))
}

/// El gabinete MSZIP con los archivos, cada uno con el nombre de su fila en `File`, en el orden de
/// `Sequence`.
fn cabinet(files: &[MsiFile]) -> Result<Vec<u8>, String> {
    let io = |e: std::io::Error| format!("cab: {e}");
    let mut builder = cab::CabinetBuilder::new();
    // Fecha fija: paquetes reproducibles.
    let when = time::Date::from_calendar_date(2020, time::Month::January, 1)
        .map(|d| time::PrimitiveDateTime::new(d, time::Time::MIDNIGHT))
        .map_err(|e| format!("cab: {e}"))?;
    {
        let folder = builder.add_folder(cab::CompressionType::MsZip);
        for f in files {
            folder.add_file(key(&f.name)).set_datetime(when);
        }
    }
    let mut writer = builder.build(Cursor::new(Vec::new())).map_err(io)?;
    let mut k = 0usize;
    while let Some(mut w) = writer.next_file().map_err(io)? {
        let f = files.get(k).ok_or("cab: más archivos de los esperados")?;
        w.write_all(&f.bytes).map_err(io)?;
        k += 1;
    }
    Ok(writer.finish().map_err(io)?.into_inner())
}

fn uuid_of(g: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(g.trim_matches(['{', '}'])).map_err(|e| format!("msi: GUID {g}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const ID: &str = "com.iurefficient.IureDesk";

    #[test]
    fn product_versions_order_like_the_releases() {
        let v = |s: &str| product_version(s).unwrap();
        assert_eq!(v("0.1.0-alpha.9"), "0.1.9");
        assert_eq!(v("0.1.0-beta.1"), "0.1.301");
        assert_eq!(v("0.1.0-rc.2"), "0.1.602");
        assert_eq!(v("0.1.0"), "0.1.999");
        assert_eq!(v("0.1.1-alpha.1"), "0.1.1001");
        let key = |s: &str| v(s).split('.').map(|p| p.parse::<u32>().unwrap()).collect::<Vec<_>>();
        let order = ["0.1.0-alpha.8", "0.1.0-alpha.9", "0.1.0-beta.1", "0.1.0-rc.1", "0.1.0", "0.1.1-alpha.1", "0.2.0"];
        assert!(order.windows(2).all(|w| key(w[0]) < key(w[1])), "{order:?}");
        assert!(product_version("0.1.0-gamma.1").is_err());
        assert!(product_version("0.1.66-alpha.1").is_err());
        assert!(product_version("x").is_err());
    }

    #[test]
    fn guids_are_stable_and_well_formed() {
        let g = guid("upgrade");
        assert_eq!(g, guid("upgrade"));
        assert_ne!(g, guid("product/0.1.0"));
        assert_eq!(g.len(), 38);
        assert!(uuid_of(&g).is_ok());
    }

    fn sample() -> Vec<MsiFile> {
        vec![
            MsiFile { name: "iuredesk.exe".into(), bytes: b"MZ programa".to_vec() },
            MsiFile { name: "iuredesk.ico".into(), bytes: b"icono".to_vec() },
            MsiFile { name: "THIRD-PARTY-NOTICES.txt".into(), bytes: b"avisos".repeat(100) },
        ]
    }

    /// El paquete se vuelve a abrir con el mismo crate: tablas, filas y el gabinete con los archivos.
    #[test]
    fn msi_has_the_tables_and_the_files() {
        let files = sample();
        let bytes = build("0.1.0-alpha.9", ID, &files, b"ico").unwrap();
        let mut pkg = Package::open(Cursor::new(bytes)).unwrap();
        assert_eq!(pkg.summary_info().arch(), Some("x64"));
        assert_eq!(pkg.summary_info().word_count(), Some(2));
        assert_eq!(pkg.summary_info().page_count(), Some(500));
        for t in [
            "Property",
            "Directory",
            "Component",
            "File",
            "Feature",
            "FeatureComponents",
            "Media",
            "Shortcut",
            "MsiShortcutProperty",
            "Registry",
            "Icon",
            "Upgrade",
            "InstallExecuteSequence",
            "InstallUISequence",
        ] {
            assert!(pkg.has_table(t), "{t}");
        }
        let props: Vec<(String, String)> = pkg
            .select_rows(msi::Select::table("Property"))
            .unwrap()
            .map(|r| (r[0].as_str().unwrap_or("").to_string(), r[1].as_str().unwrap_or("").to_string()))
            .collect();
        assert!(props.contains(&("ProductVersion".into(), "0.1.9".into())), "{props:?}");
        assert!(props.contains(&("ProductName".into(), "IureDesk".into())), "{props:?}");
        let mut names: Vec<String> = pkg
            .select_rows(msi::Select::table("File"))
            .unwrap()
            .map(|r| r[2].as_str().unwrap_or("").to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["FILE0.EXE|iuredesk.exe", "FILE1.ICO|iuredesk.ico", "FILE2.TXT|THIRD-PARTY-NOTICES.txt"]);
        let mut cab = Vec::new();
        pkg.read_stream(CABINET).unwrap().read_to_end(&mut cab).unwrap();
        let mut cabinet = cab::Cabinet::new(Cursor::new(cab)).unwrap();
        let mut got = Vec::new();
        cabinet.read_file("THIRD_PARTY_NOTICES.txt").unwrap().read_to_end(&mut got).unwrap();
        assert_eq!(got, files[2].bytes);
        assert!(pkg.has_stream("Icon.iuredesk.ico"));
        let aumids: Vec<(String, String)> = pkg
            .select_rows(msi::Select::table("MsiShortcutProperty"))
            .unwrap()
            .map(|r| (r[1].as_str().unwrap_or("").to_string(), r[3].as_str().unwrap_or("").to_string()))
            .collect();
        assert_eq!(aumids, [("MenuInicio".into(), ID.into()), ("Escritorio".into(), ID.into())]);
        assert!(build("0.1.0", ID, &files[1..], b"").is_err(), "sin iuredesk.exe primero");
        assert!(build("0.1.0", ID, &files[..1], b"").is_err(), "sin el .ico");
    }

    /// Lo mismo que registra `default-apps.nsh`, con el comando entre comillas.
    #[test]
    fn registry_matches_the_nsis_integration() {
        let bytes = build("0.1.0-alpha.9", ID, &sample(), b"ico").unwrap();
        let mut pkg = Package::open(Cursor::new(bytes)).unwrap();
        let rows: Vec<(i32, String, String, String)> = pkg
            .select_rows(msi::Select::table("Registry"))
            .unwrap()
            .map(|r| {
                (
                    r[1].as_int().unwrap_or(-1),
                    r[2].as_str().unwrap_or("").to_string(),
                    r[3].as_str().unwrap_or("").to_string(),
                    r[4].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        let has = |root: i32, k: &str, n: &str, v: &str| {
            rows.iter().any(|(r, kk, nn, vv)| *r == root && kk == k && nn == n && vv == v)
        };
        let open = "\"[#iuredesk.exe]\" \"%1\"";
        assert!(has(0, "iuredesk", "URL Protocol", ""), "{rows:?}");
        assert!(has(0, "iuredesk\\shell\\open\\command", "", open));
        assert!(has(0, "IureDesk.mailto", "URL Protocol", ""));
        assert!(has(0, "IureDesk.mailto\\shell\\open\\command", "", open));
        assert!(has(0, &format!("AppUserModelId\\{ID}"), "IconUri", "[#iuredesk.ico]"));
        for ext in ["eml", "ics", "vcf"] {
            assert!(has(0, &format!(".{ext}\\OpenWithProgids"), &format!("IureDesk.{ext}"), ""), "{ext}");
            assert!(has(0, &format!("IureDesk.{ext}\\shell\\open\\command"), "", open), "{ext}");
            assert!(has(2, &format!("{CAPS_KEY}\\FileAssociations"), &format!(".{ext}"), &format!("IureDesk.{ext}")));
        }
        assert!(has(2, &format!("{CAPS_KEY}\\URLAssociations"), "mailto", "IureDesk.mailto"));
        assert!(has(2, "Software\\RegisteredApplications", "IureDesk", CAPS_KEY));
        assert!(has(2, SUITE_KEY, "Version", "0.1.0-alpha.9"));
        assert!(has(2, SUITE_KEY, "ProductCode", "[ProductCode]"));
        assert!(has(2, SUITE_KEY, "InstallDir", "[INSTALLDIR]"));
        // Nunca se toma HKCR\mailto ni el valor por omisión de una extensión.
        assert!(!rows.iter().any(|(_, k, _, _)| k == "mailto" || k == ".eml"), "{rows:?}");
        // Identificadores únicos.
        let mut ids: Vec<String> = pkg
            .select_rows(msi::Select::table("Registry"))
            .unwrap()
            .map(|r| r[0].as_str().unwrap_or("").to_string())
            .collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }
}
