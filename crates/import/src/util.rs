//! Utilidades: lectura de líneas acotada, decodificación de texto heredado y recorrido de
//! directorios con límites.

use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

/// Caracteres de Windows-1252 para 0x80–0x9F (el resto coincide con Latin-1).
const CP1252_HIGH: [char; 32] = [
    '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘', '’',
    '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
];

/// Windows-1252 → texto.
pub fn decode_cp1252(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            0x80..=0x9F => CP1252_HIGH.get(usize::from(c - 0x80)).copied().unwrap_or('\u{fffd}'),
            c => char::from(c),
        })
        .collect()
}

/// UTF-8 si es válido; si no, Windows-1252 (lo habitual en exportaciones antiguas de Outlook).
pub fn decode_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => decode_cp1252(b),
    }
}

/// UTF-16 (con BOM) → texto, si lo parece.
pub fn decode_utf16_bom(b: &[u8]) -> Option<String> {
    let le = match b.get(..2) {
        Some([0xFF, 0xFE]) => true,
        Some([0xFE, 0xFF]) => false,
        _ => return None,
    };
    let units: Vec<u16> = b
        .get(2..)
        .unwrap_or(&[])
        .chunks_exact(2)
        .map(|c| {
            let pair = [c.first().copied().unwrap_or(0), c.get(1).copied().unwrap_or(0)];
            if le { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) }
        })
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// Lee una línea (con su `\n`) en `buf`, guardando como mucho `cap` bytes; el resto de una línea
/// más larga se descarta. Devuelve los bytes consumidos (0 = fin) y si se truncó.
pub fn read_line_capped<R: BufRead>(r: &mut R, buf: &mut Vec<u8>, cap: usize) -> io::Result<(u64, bool)> {
    buf.clear();
    let mut consumed: u64 = 0;
    let mut truncated = false;
    loop {
        let data = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if data.is_empty() {
            return Ok((consumed, truncated));
        }
        let (take, found) = match data.iter().position(|&b| b == b'\n') {
            Some(i) => (i.saturating_add(1), true),
            None => (data.len(), false),
        };
        let room = cap.saturating_sub(buf.len());
        if room < take {
            truncated = true;
        }
        buf.extend_from_slice(data.get(..take.min(room)).unwrap_or(&[]));
        r.consume(take);
        consumed = consumed.saturating_add(take as u64);
        if found {
            return Ok((consumed, truncated));
        }
    }
}

/// Recorrido de directorio acotado.
#[derive(Debug)]
pub struct Walk {
    /// Archivos encontrados (ruta, tamaño).
    pub files: Vec<(PathBuf, u64)>,
    /// Se alcanzó el límite de archivos o de profundidad.
    pub truncated: bool,
}

/// Lista los archivos bajo `root` (sin seguir enlaces simbólicos), como mucho `max_files`, hasta
/// `max_depth` niveles, en orden de nombre.
pub fn walk_files(root: &Path, max_files: usize, max_depth: usize) -> Walk {
    let mut out = Walk { files: Vec::new(), truncated: false };
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        let mut subdirs = Vec::new();
        for e in entries {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if depth >= max_depth {
                    out.truncated = true;
                } else {
                    subdirs.push(e.path());
                }
            } else if ft.is_file() {
                if out.files.len() >= max_files {
                    out.truncated = true;
                    return out;
                }
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.files.push((e.path(), len));
            }
        }
        for d in subdirs.into_iter().rev() {
            stack.push((d, depth.saturating_add(1)));
        }
    }
    out
}

/// Subdirectorios directos (sin enlaces), en orden de nombre.
pub fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> =
        rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).take(100_000).collect();
    v.sort();
    v
}

/// Archivos directos (sin enlaces), en orden de nombre.
pub fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> =
        rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())).map(|e| e.path()).take(1_000_000).collect();
    v.sort();
    v
}

/// Nombre de archivo como texto.
pub fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Extensión en minúsculas.
pub fn ext(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// Lee los primeros `n` bytes de un archivo.
pub fn head(p: &Path, n: usize) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    if let Ok(f) = std::fs::File::open(p) {
        let _ = f.take(n as u64).read_to_end(&mut out);
    }
    out
}

/// Lee un archivo entero si no supera `max` bytes.
pub fn read_bounded(p: &Path, max: u64) -> Result<Vec<u8>, ReadError> {
    let len = std::fs::metadata(p).map_err(|e| ReadError::Io(e.to_string()))?.len();
    if len > max {
        return Err(ReadError::TooLarge(len));
    }
    std::fs::read(p).map_err(|e| ReadError::Io(e.to_string()))
}

/// Error de [`read_bounded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// Supera el máximo (tamaño real).
    TooLarge(u64),
    /// E/S.
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_legacy_text() {
        assert_eq!(decode_text("Peña".as_bytes()), "Peña");
        assert_eq!(decode_text(b"Pe\xf1a \x80"), "Peña €");
        assert_eq!(decode_utf16_bom(&[0xFF, 0xFE, b'h', 0, b'i', 0]).as_deref(), Some("hi"));
        assert_eq!(decode_utf16_bom(b"hi"), None);
    }

    #[test]
    fn capped_lines() {
        let mut r = std::io::Cursor::new(b"corta\nmuy larga linea\nfin".to_vec());
        let mut b = Vec::new();
        assert_eq!(read_line_capped(&mut r, &mut b, 100).unwrap(), (6, false));
        assert_eq!(b, b"corta\n");
        assert_eq!(read_line_capped(&mut r, &mut b, 4).unwrap(), (16, true));
        assert_eq!(b, b"muy ");
        assert_eq!(read_line_capped(&mut r, &mut b, 100).unwrap(), (3, false));
        assert_eq!(b, b"fin");
        assert_eq!(read_line_capped(&mut r, &mut b, 100).unwrap(), (0, false));
    }

    #[test]
    fn walk_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        std::fs::write(dir.path().join("a/1.eml"), b"x").unwrap();
        std::fs::write(dir.path().join("a/b/c/2.eml"), b"x").unwrap();
        let w = walk_files(dir.path(), 10, 10);
        assert_eq!(w.files.len(), 2);
        assert!(!w.truncated);
        let w = walk_files(dir.path(), 10, 1);
        assert_eq!(w.files.len(), 1);
        assert!(w.truncated);
        assert!(walk_files(dir.path(), 1, 10).truncated);
    }
}
