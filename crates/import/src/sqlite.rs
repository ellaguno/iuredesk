//! Lector mínimo, de solo lectura, del formato de archivo de SQLite 3 (para `abook.sqlite` de
//! Thunderbird).
//!
//! **Por qué propio.** El proyecto no usa la biblioteca C de SQLite (`docs/excepciones.md`). Los
//! lectores en Rust puro de crates.io (2026-10: `sqlite-core`, `sqlite-rs`, `sootmark-sqlite`,
//! `graphitesql`…) son jóvenes, con pocos usuarios, y varios hacen `unwrap` sobre datos del archivo.
//! Lo que necesitamos es pequeño y está bien especificado (<https://www.sqlite.org/fileformat2.html>):
//! recorrer el árbol B de una tabla, leer celdas con desbordamiento y decodificar registros. Este
//! lector lo hace con límites (páginas visitadas, profundidad, filas, tamaño) y sin pánicos.
//!
//! También aplica el registro WAL (`-wal`) si existe: con Thunderbird abierto (o tras un cierre
//! brusco) los últimos cambios pueden estar solo ahí. Se toman las tramas válidas (sal y suma de control)
//! hasta la última confirmación.
//!
//! No implementa: índices, tablas `WITHOUT ROWID`, el journal de reversión (`-journal`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Valor de una columna.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `NULL`.
    Null,
    /// Entero.
    Int(i64),
    /// Real.
    Real(f64),
    /// Texto.
    Text(String),
    /// Binario.
    Blob(Vec<u8>),
}

impl Value {
    /// Como texto (enteros y reales se formatean; binario como UTF-8 con pérdida).
    pub fn as_text(&self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Int(i) => Some(i.to_string()),
            Self::Real(r) => Some(r.to_string()),
            Self::Text(t) => Some(t.clone()),
            Self::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
        }
    }
}

/// Tabla del esquema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableInfo {
    /// Nombre.
    pub name: String,
    /// Página raíz.
    pub root: u32,
    /// Columnas (en orden).
    pub columns: Vec<String>,
    /// Índice de la columna `INTEGER PRIMARY KEY` (alias del rowid), si la hay.
    rowid_alias: Option<usize>,
}

/// Error del lector.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SqliteError {
    /// No es una base de datos SQLite 3.
    #[error("no es una base de datos SQLite")]
    NotSqlite,
    /// Estructura dañada.
    #[error("base de datos dañada: {0}")]
    Corrupt(String),
    /// Límite de seguridad.
    #[error("límite superado: {0}")]
    Limit(&'static str),
    /// No existe la tabla.
    #[error("no existe la tabla {0}")]
    NoTable(String),
    /// E/S.
    #[error("{0}")]
    Io(String),
}

type Res<T> = Result<T, SqliteError>;

/// Páginas máximas que se visitan en un recorrido.
const MAX_PAGES_VISITED: usize = 2_000_000;
/// Profundidad máxima del árbol B.
const MAX_DEPTH: usize = 40;
/// Tamaño máximo de una celda reconstruida.
const MAX_PAYLOAD: u64 = 64 * 1024 * 1024;

/// Base de datos abierta en memoria.
pub struct SqliteDb {
    data: Vec<u8>,
    page_size: usize,
    usable: usize,
    encoding: u32,
    wal: HashMap<u32, Vec<u8>>,
    pages: u32,
}

impl std::fmt::Debug for SqliteDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteDb").field("page_size", &self.page_size).field("pages", &self.pages).finish()
    }
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// Varint de SQLite: (valor, bytes usados).
fn varint(b: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut v: u64 = 0;
    for i in 0..9usize {
        let byte = *b.get(at.checked_add(i)?)?;
        if i == 8 {
            v = (v << 8) | u64::from(byte);
            return Some((v, 9));
        }
        v = (v << 7) | u64::from(byte & 0x7F);
        if byte & 0x80 == 0 {
            return Some((v, i.saturating_add(1)));
        }
    }
    None
}

impl SqliteDb {
    /// Abre `path` (y `path-wal` si existe). Rechaza archivos de más de `max_bytes`.
    pub fn open(path: &Path, max_bytes: u64) -> Res<Self> {
        let len = std::fs::metadata(path).map_err(|e| SqliteError::Io(e.to_string()))?.len();
        if len > max_bytes {
            return Err(SqliteError::Limit("tamaño del archivo"));
        }
        let data = std::fs::read(path).map_err(|e| SqliteError::Io(e.to_string()))?;
        let mut wal_path = path.as_os_str().to_owned();
        wal_path.push("-wal");
        let wal = match std::fs::metadata(&wal_path) {
            Ok(m) if m.len() > 0 && m.len() <= max_bytes => std::fs::read(&wal_path).ok(),
            _ => None,
        };
        Self::from_bytes(data, wal)
    }

    /// Desde bytes (base de datos y, opcionalmente, su WAL).
    pub fn from_bytes(data: Vec<u8>, wal: Option<Vec<u8>>) -> Res<Self> {
        if data.len() < 100 || !data.starts_with(b"SQLite format 3\0") {
            return Err(SqliteError::NotSqlite);
        }
        let raw_ps = be16(&data, 16).ok_or(SqliteError::NotSqlite)?;
        let page_size: usize = if raw_ps == 1 { 65_536 } else { usize::from(raw_ps) };
        if page_size < 512 || !page_size.is_power_of_two() {
            return Err(SqliteError::Corrupt("tamaño de página".into()));
        }
        let reserved = usize::from(*data.get(20).ok_or(SqliteError::NotSqlite)?);
        let usable = page_size.saturating_sub(reserved);
        if usable < 480 {
            return Err(SqliteError::Corrupt("espacio reservado".into()));
        }
        let encoding = be32(&data, 56).unwrap_or(1);
        let file_pages = u32::try_from(data.len() / page_size).unwrap_or(u32::MAX);
        let mut db = Self { data, page_size, usable, encoding, wal: HashMap::new(), pages: file_pages };
        if let Some(w) = wal {
            db.apply_wal(&w);
        }
        Ok(db)
    }

    fn apply_wal(&mut self, w: &[u8]) {
        let Some(magic) = be32(w, 0) else { return };
        let little = match magic {
            0x377f_0682 => true,
            0x377f_0683 => false,
            _ => return,
        };
        let Some(ps) = be32(w, 8).and_then(|p| usize::try_from(p).ok()) else { return };
        if ps != self.page_size {
            return;
        }
        let (Some(salt1), Some(salt2)) = (be32(w, 16), be32(w, 20)) else { return };
        let Some(hdr) = w.get(..24) else { return };
        let mut ck = wal_checksum(hdr, (0, 0), little);
        if Some(ck.0) != be32(w, 24) || Some(ck.1) != be32(w, 28) {
            return;
        }
        let mut pending: Vec<(u32, &[u8])> = Vec::new();
        let mut committed: HashMap<u32, Vec<u8>> = HashMap::new();
        let mut db_pages = self.pages;
        let mut at = 32usize;
        let frame = ps.saturating_add(24);
        while let Some(f) = w.get(at..at.saturating_add(frame)) {
            let (Some(pgno), Some(commit), Some(s1), Some(s2)) = (be32(f, 0), be32(f, 4), be32(f, 8), be32(f, 12))
            else {
                break;
            };
            if s1 != salt1 || s2 != salt2 || pgno == 0 {
                break;
            }
            let page = f.get(24..).unwrap_or(&[]);
            ck = wal_checksum(f.get(..8).unwrap_or(&[]), ck, little);
            ck = wal_checksum(page, ck, little);
            if Some(ck.0) != be32(f, 16) || Some(ck.1) != be32(f, 20) {
                break;
            }
            pending.push((pgno, page));
            if commit != 0 {
                for (p, d) in pending.drain(..) {
                    committed.insert(p, d.to_vec());
                }
                db_pages = commit;
            }
            at = at.saturating_add(frame);
        }
        self.wal = committed;
        self.pages = db_pages;
    }

    fn page(&self, n: u32) -> Res<&[u8]> {
        if n == 0 || n > self.pages {
            return Err(SqliteError::Corrupt(format!("página {n} fuera de rango")));
        }
        if let Some(p) = self.wal.get(&n) {
            return Ok(p);
        }
        let start = (n as usize).saturating_sub(1).saturating_mul(self.page_size);
        self.data
            .get(start..start.saturating_add(self.page_size))
            .ok_or_else(|| SqliteError::Corrupt(format!("página {n} truncada")))
    }

    /// Tablas del esquema.
    pub fn tables(&self) -> Res<Vec<TableInfo>> {
        let rows = self.scan(1, 100_000)?;
        let mut out = Vec::new();
        for (_, row) in rows {
            let text = |i: usize| row.get(i).and_then(Value::as_text).unwrap_or_default();
            if text(0) != "table" {
                continue;
            }
            let root = match row.get(3) {
                Some(Value::Int(r)) => u32::try_from(*r).unwrap_or(0),
                _ => 0,
            };
            if root == 0 {
                continue;
            }
            let sql = text(4);
            let (columns, rowid_alias) = columns_from_sql(&sql);
            out.push(TableInfo { name: text(1), root, columns, rowid_alias });
        }
        Ok(out)
    }

    /// Filas de una tabla (como mucho `max_rows`), con el nombre de cada columna.
    pub fn rows(&self, table: &str, max_rows: usize) -> Res<(Vec<String>, Vec<Vec<Value>>)> {
        let t = self
            .tables()?
            .into_iter()
            .find(|t| t.name.eq_ignore_ascii_case(table))
            .ok_or_else(|| SqliteError::NoTable(table.to_string()))?;
        let rows = self.scan(t.root, max_rows)?;
        let out = rows
            .into_iter()
            .map(|(rowid, mut r)| {
                if let Some(i) = t.rowid_alias
                    && let Some(v) = r.get_mut(i)
                    && *v == Value::Null
                {
                    *v = Value::Int(rowid);
                }
                r.resize(t.columns.len().max(r.len()), Value::Null);
                r
            })
            .collect();
        Ok((t.columns, out))
    }

    /// Recorre el árbol B de tabla con raíz `root`.
    fn scan(&self, root: u32, max_rows: usize) -> Res<Vec<(i64, Vec<Value>)>> {
        let mut out = Vec::new();
        let mut stack: Vec<(u32, usize)> = vec![(root, 0)];
        let mut visited: HashSet<u32> = HashSet::new();
        while let Some((pg, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                return Err(SqliteError::Limit("profundidad del árbol"));
            }
            if !visited.insert(pg) || visited.len() > MAX_PAGES_VISITED {
                return Err(SqliteError::Corrupt("ciclo de páginas".into()));
            }
            let page = self.page(pg)?;
            let h = if pg == 1 { 100 } else { 0 };
            let kind = *page.get(h).ok_or_else(|| SqliteError::Corrupt("cabecera de página".into()))?;
            let ncells = usize::from(be16(page, h.saturating_add(3)).unwrap_or(0));
            match kind {
                0x0D => {
                    for i in 0..ncells {
                        let ptr = be16(page, h.saturating_add(8).saturating_add(i.saturating_mul(2)))
                            .ok_or_else(|| SqliteError::Corrupt("puntero de celda".into()))?;
                        if let Some((rowid, rec)) = self.leaf_cell(page, usize::from(ptr))? {
                            out.push((rowid, self.record(&rec)?));
                            if out.len() >= max_rows {
                                return Ok(out);
                            }
                        }
                    }
                }
                0x05 => {
                    let right = be32(page, h.saturating_add(8)).unwrap_or(0);
                    // En orden: hijos izquierdos y luego el de la derecha (pila → al revés).
                    let mut children = Vec::with_capacity(ncells.saturating_add(1));
                    for i in 0..ncells {
                        let ptr = be16(page, h.saturating_add(12).saturating_add(i.saturating_mul(2)))
                            .ok_or_else(|| SqliteError::Corrupt("puntero de celda".into()))?;
                        if let Some(child) = be32(page, usize::from(ptr)) {
                            children.push(child);
                        }
                    }
                    children.push(right);
                    for c in children.into_iter().rev() {
                        stack.push((c, depth.saturating_add(1)));
                    }
                }
                _ => return Err(SqliteError::Corrupt(format!("tipo de página {kind:#x} en tabla"))),
            }
        }
        Ok(out)
    }

    /// Celda de hoja de tabla: rowid y carga completa (siguiendo el desbordamiento).
    fn leaf_cell(&self, page: &[u8], at: usize) -> Res<Option<(i64, Vec<u8>)>> {
        let bad = || SqliteError::Corrupt("celda".into());
        let (p, n1) = varint(page, at).ok_or_else(bad)?;
        let (rowid, n2) = varint(page, at.saturating_add(n1)).ok_or_else(bad)?;
        if p > MAX_PAYLOAD {
            return Err(SqliteError::Limit("tamaño de celda"));
        }
        let start = at.saturating_add(n1).saturating_add(n2);
        let u = self.usable;
        let x = u.saturating_sub(35);
        let p_us = usize::try_from(p).map_err(|_| bad())?;
        let local = if p_us <= x {
            p_us
        } else {
            let m = (u.saturating_sub(12).saturating_mul(32) / 255).saturating_sub(23);
            let k = m.saturating_add(p_us.saturating_sub(m) % u.saturating_sub(4).max(1));
            if k <= x { k } else { m }
        };
        let mut out = Vec::with_capacity(p_us.min(1 << 20));
        out.extend_from_slice(page.get(start..start.saturating_add(local)).ok_or_else(bad)?);
        if local < p_us {
            let mut next = be32(page, start.saturating_add(local)).ok_or_else(bad)?;
            let mut guard = 0usize;
            while out.len() < p_us {
                guard = guard.saturating_add(1);
                if next == 0 || guard > MAX_PAGES_VISITED {
                    return Err(bad());
                }
                let ov = self.page(next)?;
                let room = p_us.saturating_sub(out.len()).min(u.saturating_sub(4));
                out.extend_from_slice(ov.get(4..4usize.saturating_add(room)).ok_or_else(bad)?);
                next = be32(ov, 0).unwrap_or(0);
            }
        }
        Ok(Some((rowid as i64, out)))
    }

    fn text(&self, b: &[u8]) -> String {
        match self.encoding {
            2 | 3 => {
                let units: Vec<u16> = b
                    .chunks_exact(2)
                    .map(|c| {
                        let pair = [c.first().copied().unwrap_or(0), c.get(1).copied().unwrap_or(0)];
                        if self.encoding == 2 { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) }
                    })
                    .collect();
                String::from_utf16_lossy(&units)
            }
            _ => String::from_utf8_lossy(b).into_owned(),
        }
    }

    /// Decodifica un registro.
    fn record(&self, rec: &[u8]) -> Res<Vec<Value>> {
        let bad = || SqliteError::Corrupt("registro".into());
        let (hlen, n) = varint(rec, 0).ok_or_else(bad)?;
        let hlen = usize::try_from(hlen).map_err(|_| bad())?;
        let mut types = Vec::new();
        let mut at = n;
        while at < hlen {
            let (t, k) = varint(rec, at).ok_or_else(bad)?;
            types.push(t);
            at = at.saturating_add(k);
            if types.len() > 2_000 {
                return Err(SqliteError::Limit("columnas"));
            }
        }
        let mut body = hlen;
        let mut out = Vec::with_capacity(types.len());
        for t in types {
            let int = |len: usize, body: usize| -> Option<i64> {
                let bytes = rec.get(body..body.checked_add(len)?)?;
                let mut v: i64 = if bytes.first().is_some_and(|b| b & 0x80 != 0) { -1 } else { 0 };
                for b in bytes {
                    v = (v << 8) | i64::from(*b);
                }
                Some(v)
            };
            let (val, len) = match t {
                0 => (Value::Null, 0),
                1..=4 => {
                    let len = t as usize;
                    (Value::Int(int(len, body).ok_or_else(bad)?), len)
                }
                5 => (Value::Int(int(6, body).ok_or_else(bad)?), 6),
                6 => (Value::Int(int(8, body).ok_or_else(bad)?), 8),
                7 => {
                    let bytes: [u8; 8] =
                        rec.get(body..body.saturating_add(8)).and_then(|s| s.try_into().ok()).ok_or_else(bad)?;
                    (Value::Real(f64::from_be_bytes(bytes)), 8)
                }
                8 => (Value::Int(0), 0),
                9 => (Value::Int(1), 0),
                10 | 11 => return Err(bad()),
                n => {
                    let len = usize::try_from((n.saturating_sub(12)) / 2).map_err(|_| bad())?;
                    let bytes = rec.get(body..body.saturating_add(len)).ok_or_else(bad)?;
                    if n % 2 == 0 { (Value::Blob(bytes.to_vec()), len) } else { (Value::Text(self.text(bytes)), len) }
                }
            };
            out.push(val);
            body = body.saturating_add(len);
        }
        Ok(out)
    }
}

/// Suma de control de WAL (pares de palabras de 32 bits).
fn wal_checksum(data: &[u8], (mut s0, mut s1): (u32, u32), little: bool) -> (u32, u32) {
    for chunk in data.chunks_exact(8) {
        let word = |i: usize| -> u32 {
            let b: [u8; 4] = chunk.get(i..i.saturating_add(4)).and_then(|s| s.try_into().ok()).unwrap_or([0; 4]);
            if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) }
        };
        s0 = s0.wrapping_add(word(0)).wrapping_add(s1);
        s1 = s1.wrapping_add(word(4)).wrapping_add(s0);
    }
    (s0, s1)
}

/// Columnas de un `CREATE TABLE` (y la que es alias del rowid).
fn columns_from_sql(sql: &str) -> (Vec<String>, Option<usize>) {
    let (Some(open), Some(close)) = (sql.find('('), sql.rfind(')')) else { return (Vec::new(), None) };
    let inner = sql.get(open.saturating_add(1)..close).unwrap_or("");
    let mut defs = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in inner.chars() {
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                defs.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    defs.push(cur);
    let mut cols = Vec::new();
    let mut alias = None;
    for d in defs {
        let d = d.trim();
        let upper = d.to_ascii_uppercase();
        let first_word = upper.split_whitespace().next().unwrap_or("");
        if ["PRIMARY", "UNIQUE", "CHECK", "FOREIGN", "CONSTRAINT"].contains(&first_word) || d.is_empty() {
            continue;
        }
        let name = d
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c| matches!(c, '"' | '`' | '[' | ']' | '\''))
            .to_string();
        let rest: Vec<&str> = upper.split_whitespace().skip(1).collect();
        if rest.first() == Some(&"INTEGER") && upper.contains("PRIMARY KEY") {
            alias = Some(cols.len());
        }
        cols.push(name);
        if cols.len() > 2_000 {
            break;
        }
    }
    (cols, alias)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    #[test]
    fn varints() {
        assert_eq!(varint(&[0x05], 0), Some((5, 1)));
        assert_eq!(varint(&[0x81, 0x00], 0), Some((128, 2)));
        assert_eq!(varint(&[0xFF; 9], 0), Some((u64::MAX, 9)));
        assert_eq!(varint(&[0x81], 0), None);
    }

    #[test]
    fn sql_columns() {
        assert_eq!(
            columns_from_sql("CREATE TABLE properties (card TEXT, name TEXT, value TEXT)"),
            (vec!["card".into(), "name".into(), "value".into()], None)
        );
        let (c, a) = columns_from_sql(
            "CREATE TABLE \"t\" (id INTEGER PRIMARY KEY, \"x y\" TEXT DEFAULT (1,2), PRIMARY KEY(id))",
        );
        assert_eq!(c, vec!["id".to_string(), "x".to_string()]);
        assert_eq!(a, Some(0));
    }

    #[test]
    fn reads_multi_page_table_with_overflow() {
        // Generado con `tests/fixtures/make_sqlite.py` (datos sintéticos): páginas de 512 bytes,
        // 120 tarjetas (árbol B de varios niveles) y una nota larga (desbordamiento).
        let db = SqliteDb::open(&fixture("abook.sqlite"), 1 << 24).unwrap();
        let (cols, rows) = db.rows("properties", 1_000_000).unwrap();
        assert_eq!(cols, vec!["card", "name", "value"]);
        let cards: HashSet<String> = rows.iter().filter_map(|r| r[0].as_text()).collect();
        assert_eq!(cards.len(), 121);
        let long = rows.iter().find(|r| r[1].as_text().as_deref() == Some("Notes")).unwrap();
        let note = long[2].as_text().unwrap();
        assert_eq!(note.len(), 3001);
        assert!(note.starts_with("Nota larga"));
        let (cols, lists) = db.rows("lists", 100).unwrap();
        assert_eq!(cols[0], "uid");
        assert_eq!(lists.len(), 1);
    }

    #[test]
    fn applies_wal() {
        // Base sin checkpoint + su WAL: la tarjeta «wal-card» solo está en el WAL.
        let db = SqliteDb::open(&fixture("wal.sqlite"), 1 << 24).unwrap();
        let (_, rows) = db.rows("properties", 1000).unwrap();
        assert!(rows.iter().any(|r| r[0].as_text().as_deref() == Some("wal-card")));
        let plain = SqliteDb::from_bytes(std::fs::read(fixture("wal.sqlite")).unwrap(), None).unwrap();
        let has = plain
            .rows("properties", 1000)
            .map(|(_, rows)| rows.iter().any(|r| r[0].as_text().as_deref() == Some("wal-card")))
            .unwrap_or(false);
        assert!(!has);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(SqliteDb::from_bytes(vec![0; 200], None).unwrap_err(), SqliteError::NotSqlite);
        let mut bad = b"SQLite format 3\0".to_vec();
        bad.resize(1024, 0xFF);
        assert!(SqliteDb::from_bytes(bad, None).is_err());
    }

    proptest::proptest! {
        #[test]
        fn corrupted_pages_never_panic(seed in proptest::collection::vec((0usize..4096, proptest::num::u8::ANY), 1..40)) {
            let mut data = std::fs::read(fixture("abook.sqlite")).unwrap();
            for (pos, b) in seed {
                let len = data.len();
                if let Some(x) = data.get_mut(pos % len) {
                    *x = b;
                }
            }
            if let Ok(db) = SqliteDb::from_bytes(data, None) {
                let _ = db.rows("properties", 10_000);
            }
        }
    }
}
