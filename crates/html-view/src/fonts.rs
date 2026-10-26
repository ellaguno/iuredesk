//! Fuentes en Rust puro.
//!
//! La feature `system-fonts` de Blitz (fontique «system») usa `yeslogic-fontconfig-sys` en Linux,
//! que es C. En su lugar se escanean los directorios de fuentes conocidos con el escáner de
//! fontique (lee `name`/`OS/2` de cada archivo; Rust puro) y se fijan a mano las familias genéricas
//! (`sans-serif`, `serif`, `monospace`, `emoji`…) y los respaldos por escritura (CJK, árabe,
//! hebreo…), que es lo que fontconfig resolvería.
//!
//! El escaneo se hace una vez por proceso ([`warm_up`]); cada documento recibe un clon.
//! `IUREDESK_FONT_DIRS` (lista separada por `:` o `;`) añade directorios, p. ej. fuentes
//! empaquetadas con la app como respaldo garantizado.

use std::path::PathBuf;
use std::sync::OnceLock;

use parley::FontContext;
use parley::fontique::{Collection, CollectionOptions, FamilyId, GenericFamily, Script, SourceCache};

static CONTEXT: OnceLock<FontContext> = OnceLock::new();

pub(crate) fn warm_up() {
    let _ = CONTEXT.get_or_init(build);
}

/// Contexto de fuentes listo para un documento.
pub(crate) fn font_context() -> FontContext {
    CONTEXT.get_or_init(build).clone()
}

const SANS: &[&str] = &[
    "Arial",
    "Liberation Sans",
    "Arimo",
    "Helvetica",
    "Noto Sans",
    "DejaVu Sans",
    "Ubuntu",
    "Cantarell",
    "Open Sans",
    "Roboto",
    "Lato",
    "FreeSans",
    "Segoe UI",
];
const SERIF: &[&str] =
    &["Times New Roman", "Liberation Serif", "Tinos", "Noto Serif", "DejaVu Serif", "FreeSerif", "Georgia"];
const MONO: &[&str] = &[
    "Courier New",
    "Liberation Mono",
    "Cousine",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Ubuntu Mono",
    "FreeMono",
    "Consolas",
    "Menlo",
];
const EMOJI: &[&str] = &["Noto Color Emoji", "Apple Color Emoji", "Segoe UI Emoji", "Twemoji", "EmojiOne", "Symbola"];
const CURSIVE: &[&str] = &["Comic Sans MS", "Comic Neue", "URW Chancery L", "Z003"];
const FANTASY: &[&str] = &["Impact", "Papyrus"];
const CJK_JP: &[&str] = &[
    "Noto Sans CJK JP",
    "Source Han Sans JP",
    "IPAGothic",
    "IPAPGothic",
    "TakaoPGothic",
    "Hiragino Sans",
    "Yu Gothic",
    "MS Gothic",
];
const CJK_SC: &[&str] = &[
    "Noto Sans CJK SC",
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
    "WenQuanYi Zen Hei",
    "Droid Sans Fallback",
    "PingFang SC",
    "Microsoft YaHei",
];
const CJK_KR: &[&str] =
    &["Noto Sans CJK KR", "Source Han Sans KR", "NanumGothic", "Malgun Gothic", "Apple SD Gothic Neo"];
const ARABIC: &[&str] = &["Noto Sans Arabic", "Noto Naskh Arabic", "DejaVu Sans", "Arial", "Geeza Pro"];
const HEBREW: &[&str] = &["Noto Sans Hebrew", "DejaVu Sans", "Arial", "FreeSans"];
const THAI: &[&str] = &["Noto Sans Thai", "Loma", "Garuda", "Tahoma", "Thonburi"];
const DEVANAGARI: &[&str] = &["Noto Sans Devanagari", "Lohit Devanagari", "Mangal", "FreeSans"];
const SYMBOLS: &[&str] = &["DejaVu Sans", "Noto Sans Symbols", "Noto Sans Symbols2", "Symbola", "FreeSerif"];

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(extra) = std::env::var_os("IUREDESK_FONT_DIRS") {
        let extra = extra.to_string_lossy().into_owned();
        dirs.extend(extra.split([':', ';']).filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        dirs.push("/System/Library/Fonts".into());
        dirs.push("/Library/Fonts".into());
        if let Some(h) = &home {
            dirs.push(h.join("Library/Fonts"));
        }
    } else if cfg!(windows) {
        let win = std::env::var_os("WINDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        dirs.push(win.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft\\Windows\\Fonts"));
        }
    } else {
        dirs.push("/usr/share/fonts".into());
        dirs.push("/usr/local/share/fonts".into());
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".local/share")));
        if let Some(d) = data_home {
            dirs.push(d.join("fonts"));
        }
        if let Some(h) = &home {
            dirs.push(h.join(".fonts"));
        }
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

fn build() -> FontContext {
    let mut collection = Collection::new(CollectionOptions { shared: false, system_fonts: false });
    collection.load_fonts_from_paths(font_dirs());
    // La fuente de viñetas de Blitz (para `list-style`), siempre disponible.
    collection.register_fonts(blitz_dom::BULLET_FONT.to_vec().into(), None);

    let sans = ids(&mut collection, SANS);
    let serif = ids(&mut collection, SERIF);
    let mono = ids(&mut collection, MONO);
    let emoji = ids(&mut collection, EMOJI);
    let any_sans = if sans.is_empty() { first_families(&mut collection) } else { sans.clone() };

    set_generic(&mut collection, GenericFamily::SansSerif, &any_sans);
    set_generic(&mut collection, GenericFamily::SystemUi, &any_sans);
    set_generic(&mut collection, GenericFamily::UiSansSerif, &any_sans);
    set_generic(&mut collection, GenericFamily::UiRounded, &any_sans);
    set_generic(&mut collection, GenericFamily::Serif, if serif.is_empty() { &any_sans } else { &serif });
    set_generic(&mut collection, GenericFamily::UiSerif, if serif.is_empty() { &any_sans } else { &serif });
    set_generic(&mut collection, GenericFamily::Monospace, if mono.is_empty() { &any_sans } else { &mono });
    set_generic(&mut collection, GenericFamily::UiMonospace, if mono.is_empty() { &any_sans } else { &mono });
    let cursive = ids(&mut collection, CURSIVE);
    set_generic(&mut collection, GenericFamily::Cursive, if cursive.is_empty() { &any_sans } else { &cursive });
    let fantasy = ids(&mut collection, FANTASY);
    set_generic(&mut collection, GenericFamily::Fantasy, if fantasy.is_empty() { &any_sans } else { &fantasy });
    set_generic(&mut collection, GenericFamily::Emoji, &emoji);

    // Respaldos por escritura: lo específico primero, luego sans y emoji.
    let jp = ids(&mut collection, CJK_JP);
    let sc = ids(&mut collection, CJK_SC);
    let kr = ids(&mut collection, CJK_KR);
    let cjk: Vec<FamilyId> = sc.iter().chain(&jp).chain(&kr).copied().collect();
    let tables: [(&[u8; 4], Vec<FamilyId>); 14] = [
        (b"Latn", Vec::new()),
        (b"Grek", Vec::new()),
        (b"Cyrl", Vec::new()),
        (b"Zyyy", ids(&mut collection, SYMBOLS)),
        (b"Zinh", Vec::new()),
        (b"Arab", ids(&mut collection, ARABIC)),
        (b"Hebr", ids(&mut collection, HEBREW)),
        (b"Thai", ids(&mut collection, THAI)),
        (b"Deva", ids(&mut collection, DEVANAGARI)),
        (b"Hani", cjk.clone()),
        (b"Hira", jp.iter().chain(&sc).copied().collect()),
        (b"Kana", jp.iter().chain(&sc).copied().collect()),
        (b"Hang", kr.iter().chain(&sc).copied().collect()),
        (b"Bopo", cjk),
    ];
    for (script, specific) in tables {
        let mut list = specific;
        list.extend(any_sans.iter().copied());
        list.extend(emoji.iter().copied());
        dedup(&mut list);
        collection.set_fallbacks(Script::from_bytes(*script), list.into_iter());
    }
    FontContext { collection, source_cache: SourceCache::new_shared() }
}

fn ids(c: &mut Collection, names: &[&str]) -> Vec<FamilyId> {
    let mut v: Vec<FamilyId> = names.iter().filter_map(|n| c.family_id(n)).collect();
    dedup(&mut v);
    v
}

fn dedup(v: &mut Vec<FamilyId>) {
    let mut seen = Vec::with_capacity(v.len());
    v.retain(|id| {
        if seen.contains(id) {
            false
        } else {
            seen.push(*id);
            true
        }
    });
}

/// Último recurso si no hay ninguna fuente conocida: las primeras familias que haya.
fn first_families(c: &mut Collection) -> Vec<FamilyId> {
    c.family_ids().take(4).collect()
}

fn set_generic(c: &mut Collection, g: GenericFamily, ids: &[FamilyId]) {
    c.set_generic_families(g, ids.iter().copied());
}
