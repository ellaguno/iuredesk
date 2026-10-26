//! Almacén de blobs direccionado por contenido (correos crudos, adjuntos, `.ics`, `.vcf`, fotos).
//!
//! - **Huella:** SHA-256 (`sha2`, Rust puro). BLAKE3 sería más rápido, pero su crate compila C/asm
//!   con `cc` (lo rechaza `cargo xtask purity`) y para correos de pocos KB la diferencia es
//!   despreciable frente a la compresión.
//! - **Ruta:** `blobs/<2 primeros hex>/<64 hex>` (256 subdirectorios, como git).
//! - **Compresión (PLAN D4):** marcos zstd estándar escritos con `ruzstd` (Rust puro; el crate
//!   `zstd` arrastra `zstd-sys`, que es C). `ruzstd` solo implementa el nivel `Fastest` (≈ zstd -1):
//!   en texto de correo comprime ~25 % mejor que lz4 a ~45 MB/s y descomprime a ~250 MB/s, más que
//!   suficiente frente a la red. Como el formato es zstd estándar, un codificador más fuerte en el
//!   futuro no rompe nada. Si comprimir no ahorra al menos un 5 % (adjuntos ya comprimidos), se
//!   guarda sin comprimir.
//! - **Formato del archivo:** `IDB` + códec (0 = crudo, 1 = zstd) + tamaño original (u64 LE) +
//!   datos. Al leer se acota el tamaño descomprimido y se verifica la huella.
//! - **Atomicidad:** se escribe a un temporal y se renombra; un corte deja, como mucho, un temporal
//!   huérfano en `blobs/tmp`, que se limpia al abrir.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use iuredesk_model::BlobHash;
use sha2::{Digest, Sha256};

use crate::error::{Result, StoreError};

/// Tamaño máximo de un blob (sin comprimir).
pub const MAX_BLOB_SIZE: u64 = 256 * 1024 * 1024;

const MAGIC: &[u8; 3] = b"IDB";
const CODEC_RAW: u8 = 0;
const CODEC_ZSTD: u8 = 1;
const HEADER_LEN: usize = 3 + 1 + 8;

/// Almacén de blobs en disco.
#[derive(Debug)]
pub struct BlobStore {
    root: PathBuf,
    tmp_counter: AtomicU64,
}

impl BlobStore {
    /// Abre (o crea) el almacén en `root`.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tmp"))?;
        // Restos de escrituras interrumpidas.
        if let Ok(rd) = fs::read_dir(root.join("tmp")) {
            for e in rd.flatten().take(100_000) {
                let _ = fs::remove_file(e.path());
            }
        }
        Ok(Self { root, tmp_counter: AtomicU64::new(0) })
    }

    /// Huella de unos datos.
    pub fn hash(data: &[u8]) -> BlobHash {
        let d = Sha256::digest(data);
        let mut out = [0u8; 32];
        out.copy_from_slice(&d);
        BlobHash(out)
    }

    fn path(&self, h: &BlobHash) -> PathBuf {
        let hex = h.to_hex();
        let shard = hex.get(..2).unwrap_or("00");
        self.root.join(shard).join(hex)
    }

    /// `true` si el blob ya está guardado.
    pub fn contains(&self, h: &BlobHash) -> bool {
        self.path(h).is_file()
    }

    /// Guarda unos datos y devuelve su huella. Si ya existían, no vuelve a escribir.
    pub fn put(&self, data: &[u8]) -> Result<BlobHash> {
        let size = data.len() as u64;
        if size > MAX_BLOB_SIZE {
            return Err(StoreError::TooLarge { what: "blob", size, max: MAX_BLOB_SIZE });
        }
        let h = Self::hash(data);
        let path = self.path(&h);
        if path.is_file() {
            return Ok(h);
        }
        let compressed = ruzstd::encoding::compress_to_vec(data, ruzstd::encoding::CompressionLevel::Fastest);
        let worth = (compressed.len() as u64).saturating_mul(100) < size.saturating_mul(95);
        let (codec, payload): (u8, &[u8]) = if worth { (CODEC_ZSTD, &compressed) } else { (CODEC_RAW, data) };

        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join("tmp").join(format!("{}-{}-{n}", h.to_hex(), std::process::id()));
        {
            let mut f = fs::File::create(&tmp)?;
            let mut header = [0u8; HEADER_LEN];
            if let Some(dst) = header.get_mut(..3) {
                dst.copy_from_slice(MAGIC);
            }
            if let Some(dst) = header.get_mut(3) {
                *dst = codec;
            }
            if let Some(dst) = header.get_mut(4..12) {
                dst.copy_from_slice(&size.to_le_bytes());
            }
            f.write_all(&header)?;
            f.write_all(payload)?;
        }
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(h)
    }

    /// Lee un blob, lo descomprime y verifica su huella.
    pub fn get(&self, h: &BlobHash) -> Result<Vec<u8>> {
        let path = self.path(h);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(StoreError::not_found("blob", h.to_hex()));
            }
            Err(e) => return Err(e.into()),
        };
        let data = decode_file(&bytes)?;
        if Self::hash(&data) != *h {
            return Err(StoreError::Corrupt(format!("{}: la huella no coincide", path_name(&path))));
        }
        Ok(data)
    }

    /// Borra un blob (no falla si no existía).
    pub fn remove(&self, h: &BlobHash) -> Result<()> {
        match fs::remove_file(self.path(h)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Bytes ocupados en disco por los blobs (recorrido acotado).
    pub fn disk_usage(&self) -> u64 {
        dir_size(&self.root, 2)
    }
}

fn path_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn decode_file(bytes: &[u8]) -> Result<Vec<u8>> {
    let header = bytes.get(..HEADER_LEN).ok_or_else(|| StoreError::Corrupt("cabecera truncada".into()))?;
    if header.get(..3) != Some(MAGIC.as_slice()) {
        return Err(StoreError::Corrupt("cabecera desconocida".into()));
    }
    let codec = header.get(3).copied().unwrap_or(u8::MAX);
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(header.get(4..12).unwrap_or(&[0; 8]));
    let size = u64::from_le_bytes(len_bytes);
    if size > MAX_BLOB_SIZE {
        return Err(StoreError::TooLarge { what: "blob", size, max: MAX_BLOB_SIZE });
    }
    let payload = bytes.get(HEADER_LEN..).unwrap_or(&[]);
    match codec {
        CODEC_RAW => {
            if payload.len() as u64 != size {
                return Err(StoreError::Corrupt("tamaño incorrecto".into()));
            }
            Ok(payload.to_vec())
        }
        CODEC_ZSTD => {
            let dec = ruzstd::decoding::StreamingDecoder::new(payload)
                .map_err(|e| StoreError::Corrupt(format!("zstd: {e}")))?;
            let mut out = Vec::with_capacity(usize::try_from(size).unwrap_or(0).min(64 * 1024 * 1024));
            // Acotado: nunca más de lo declarado (+1 para detectar exceso).
            dec.take(size.saturating_add(1))
                .read_to_end(&mut out)
                .map_err(|e| StoreError::Corrupt(format!("zstd: {e}")))?;
            if out.len() as u64 != size {
                return Err(StoreError::Corrupt("tamaño descomprimido incorrecto".into()));
            }
            Ok(out)
        }
        other => Err(StoreError::Corrupt(format!("códec desconocido {other}"))),
    }
}

pub(crate) fn dir_size(dir: &Path, depth: u32) -> u64 {
    let Ok(rd) = fs::read_dir(dir) else { return 0 };
    let mut total = 0u64;
    for e in rd.flatten().take(10_000_000) {
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            if depth > 0 {
                total = total.saturating_add(dir_size(&e.path(), depth.saturating_sub(1)));
            }
        } else {
            total = total.saturating_add(md.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_dedup() {
        let dir = tempfile::tempdir().unwrap();
        let bs = BlobStore::open(dir.path()).unwrap();
        let text = "Hola, este es un correo de prueba. ".repeat(200);
        let h = bs.put(text.as_bytes()).unwrap();
        assert_eq!(bs.put(text.as_bytes()).unwrap(), h);
        assert!(bs.contains(&h));
        assert_eq!(bs.get(&h).unwrap(), text.as_bytes());
        // Comprimido: ocupa bastante menos.
        assert!(fs::metadata(bs.path(&h)).unwrap().len() < text.len() as u64 / 4);
        bs.remove(&h).unwrap();
        assert!(matches!(bs.get(&h), Err(StoreError::NotFound { .. })));
    }

    #[test]
    fn incompressible_is_stored_raw() {
        let dir = tempfile::tempdir().unwrap();
        let bs = BlobStore::open(dir.path()).unwrap();
        let mut x = 0x1234_5678_9abc_def0u64;
        let data: Vec<u8> = (0..10_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let h = bs.put(&data).unwrap();
        assert_eq!(fs::read(bs.path(&h)).unwrap().get(3), Some(&CODEC_RAW));
        assert_eq!(bs.get(&h).unwrap(), data);
        assert_eq!(bs.get(&BlobStore::hash(b"")).ok(), None);
        let e = bs.put(b"").unwrap();
        assert_eq!(bs.get(&e).unwrap(), b"");
    }

    #[test]
    fn corrupt_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let bs = BlobStore::open(dir.path()).unwrap();
        let h = bs.put("contenido ".repeat(100).as_bytes()).unwrap();
        let p = bs.path(&h);
        let mut bytes = fs::read(&p).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&p, &bytes).unwrap();
        assert!(bs.get(&h).is_err());
        fs::write(&p, b"IDB").unwrap();
        assert!(bs.get(&h).is_err());
        // Tamaño declarado enorme.
        let mut big = b"IDB\x01".to_vec();
        big.extend_from_slice(&u64::MAX.to_le_bytes());
        fs::write(&p, &big).unwrap();
        assert!(matches!(bs.get(&h), Err(StoreError::TooLarge { .. })));
    }

    proptest::proptest! {
        #[test]
        fn decode_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..300)) {
            let _ = decode_file(&bytes);
            let mut framed = b"IDB\x01".to_vec();
            framed.extend_from_slice(&(bytes.len() as u64 * 3).to_le_bytes());
            framed.extend_from_slice(&bytes);
            let _ = decode_file(&framed);
        }
    }
}
