//! Firmas minisign de los instaladores, en el formato del actualizador de Tauri que verifica
//! iureapps (`src/verify.rs`): el `.sig` es el texto de la firma minisign (precalculada, «ED»)
//! codificado en base64 estándar, y la clave pública del catálogo es el texto de la clave pública
//! también en base64. La firma usa el crate `minisign` (Rust puro; herramienta de compilación).
//!
//! - `cargo xtask sign <archivos…>`: escribe `<archivo>.sig` junto a cada uno. La clave secreta
//!   llega en `IUREDESK_MINISIGN_KEY` (el texto de `minisign.key` tal cual o en base64, como lo
//!   guarda Tauri; en local también `IUREDESK_MINISIGN_KEY_FILE=<ruta>`) y su contraseña, si tiene, en `IUREDESK_MINISIGN_PASSWORD`.
//! - `cargo xtask keygen <secreta> <pública>`: crea un par nuevo sin contraseña. La secreta se
//!   escribe con permisos 600 y nunca se imprime; no sobrescribe archivos existentes.

use std::io::Write;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use minisign::{KeyPair, PublicKey, SecretKey, SecretKeyBox};

use crate::Res;

/// Variable con la clave secreta.
pub const KEY_ENV: &str = "IUREDESK_MINISIGN_KEY";
/// Alternativa local: ruta del archivo con la clave secreta.
pub const KEY_FILE_ENV: &str = "IUREDESK_MINISIGN_KEY_FILE";
/// Variable con la contraseña de la clave secreta (vacía si no tiene).
pub const PASSWORD_ENV: &str = "IUREDESK_MINISIGN_PASSWORD";
/// Tamaño máximo del texto de la clave (una clave minisign ocupa ~350 bytes).
const MAX_KEY_LEN: usize = 4096;

pub fn sign(files: Vec<String>) -> Res {
    if files.is_empty() {
        return Err("uso: cargo xtask sign <archivo>… (clave en IUREDESK_MINISIGN_KEY)".into());
    }
    let key = match std::env::var(KEY_ENV).ok().filter(|k| !k.trim().is_empty()) {
        Some(k) => k,
        None => {
            let file = std::env::var(KEY_FILE_ENV).ok().filter(|f| !f.trim().is_empty()).ok_or_else(|| {
                format!("falta {KEY_ENV} (o {KEY_FILE_ENV}); sin la clave secreta no se puede firmar (ver docs/publicacion.md)")
            })?;
            std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?
        }
    };
    let password = std::env::var(PASSWORD_ENV).unwrap_or_default();
    let sk = secret_key(&key, &password)?;
    let pk = PublicKey::from_secret_key(&sk).map_err(|e| format!("clave secreta: {e}"))?;
    for f in &files {
        let path = Path::new(f);
        let sig = sign_file(&pk, &sk, path)?;
        let out = sig_path(path);
        std::fs::write(&out, sig).map_err(|e| format!("{}: {e}", out.display()))?;
        println!("sign: {}", out.display());
    }
    Ok(())
}

pub fn keygen(args: Vec<String>) -> Res {
    let [secret, public] = args.as_slice() else {
        return Err("uso: cargo xtask keygen <clave-secreta> <clave-pública>".into());
    };
    let (secret, public) = (Path::new(secret), Path::new(public));
    for p in [secret, public] {
        if p.exists() {
            return Err(format!("{} ya existe; no lo sobrescribo", p.display()));
        }
    }
    let (sk_text, pk_text) = generate()?;
    write_private(secret, sk_text.as_bytes())?;
    std::fs::write(public, &pk_text).map_err(|e| format!("{}: {e}", public.display()))?;
    println!("keygen: clave secreta en {} (600, sin contraseña)", secret.display());
    println!("keygen: clave pública en {}", public.display());
    println!("keygen: clave pública en base64 (catálogo de iureapps):\n{}", STANDARD.encode(&pk_text));
    Ok(())
}

/// Par nuevo sin contraseña: (texto de la clave secreta, texto de la clave pública).
fn generate() -> Result<(String, String), String> {
    // Con contraseña vacía el crate escribe la suma de verificación y no cifra la clave: así la
    // lee igual `minisign`, Tauri y `into_secret_key(Some(""))`.
    let kp = KeyPair::generate_encrypted_keypair(Some(String::new())).map_err(|e| e.to_string())?;
    let sk = kp.sk.to_box(Some("IureDesk minisign secret key")).map_err(|e| e.to_string())?.into_string();
    let pk = kp.pk.to_box().map_err(|e| e.to_string())?.into_string();
    Ok((sk, pk))
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Res {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Res {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Texto de una clave minisign: tal cual (empieza con «untrusted comment:») o en base64.
fn key_text(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.len() > MAX_KEY_LEN {
        return Err("la clave es demasiado larga".into());
    }
    if raw.starts_with("untrusted comment:") {
        return Ok(format!("{raw}\n"));
    }
    let bytes = STANDARD.decode(raw).map_err(|_| "la clave no es texto minisign ni base64".to_string())?;
    String::from_utf8(bytes).map_err(|_| "la clave en base64 no es texto".into())
}

fn secret_key(raw: &str, password: &str) -> Result<SecretKey, String> {
    let text = key_text(raw)?;
    SecretKeyBox::from_string(&text)
        .and_then(|b| b.into_secret_key(Some(password.to_string())))
        .map_err(|e| format!("clave secreta inválida o contraseña incorrecta: {e}"))
}

/// Clave pública en el formato del catálogo de iureapps (texto de la clave en base64).
#[cfg(test)]
fn catalog_pubkey(sk: &SecretKey) -> Result<String, String> {
    let pk = PublicKey::from_secret_key(sk).map_err(|e| e.to_string())?;
    let text = pk.to_box().map_err(|e| e.to_string())?.into_string();
    Ok(STANDARD.encode(text))
}

/// Contenido del `.sig` de `path`: la firma minisign en base64, como la escribe Tauri.
fn sign_file(pk: &PublicKey, sk: &SecretKey, path: &Path) -> Result<String, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let trusted = format!("timestamp:{ts}\tfile:{name}");
    let sig = minisign::sign(
        Some(pk),
        sk,
        std::io::BufReader::new(file),
        Some(&trusted),
        Some("signature from IureDesk secret key"),
    )
    .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(STANDARD.encode(sig.into_string()))
}

fn sig_path(path: &Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".sig");
    s.into()
}

/// Comprueba que el texto sea una clave pública minisign válida (para `docs/minisign.pub`).
#[cfg(test)]
fn parse_public(text: &str) -> Result<PublicKey, String> {
    minisign::PublicKeyBox::from_string(text)
        .and_then(minisign::PublicKeyBox::into_public_key)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lo mismo que hace iureapps (`src/verify.rs`): base64 → texto → minisign-verify por bloques.
    fn iureapps_verify(pubkey_b64: &str, sig_b64: &str, file: &Path) -> Result<(), String> {
        use std::io::Read;
        let text = |b64: &str| -> Result<String, String> {
            let bytes = STANDARD.decode(b64.trim()).map_err(|e| e.to_string())?;
            String::from_utf8(bytes).map_err(|e| e.to_string())
        };
        let pk = minisign_verify::PublicKey::decode(&text(pubkey_b64)?).map_err(|e| e.to_string())?;
        let sig = minisign_verify::Signature::decode(&text(sig_b64)?).map_err(|e| e.to_string())?;
        let mut v = pk.verify_stream(&sig).map_err(|e| e.to_string())?;
        let mut f = std::fs::File::open(file).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 4096];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            v.update(buf.get(..n).unwrap_or_default());
        }
        v.finalize().map_err(|e| e.to_string())
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("iuredesk-sign-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn signature_verifies_like_iureapps() -> Res {
        let dir = temp("ok");
        let file = dir.join("IureDesk_0.1.0-linux-x64.AppImage");
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, &data).map_err(|e| e.to_string())?;

        let (sk_text, pk_text) = generate()?;
        // La clave se acepta como texto y en base64 (como la guarda Tauri).
        for raw in [sk_text.clone(), STANDARD.encode(&sk_text)] {
            let sk = secret_key(&raw, "")?;
            let pk = PublicKey::from_secret_key(&sk).map_err(|e| e.to_string())?;
            let sig = sign_file(&pk, &sk, &file)?;
            let pubkey = catalog_pubkey(&sk)?;
            assert_eq!(pubkey, STANDARD.encode(&pk_text));
            iureapps_verify(&pubkey, &sig, &file)?;
            // El texto de la firma lleva el nombre del archivo en el comentario de confianza.
            let sig_text =
                String::from_utf8(STANDARD.decode(&sig).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            assert!(sig_text.contains("file:IureDesk_0.1.0-linux-x64.AppImage"));

            // Un byte cambiado ya no verifica.
            let bad = dir.join("alterado");
            let mut d = data.clone();
            if let Some(b) = d.get_mut(500) {
                *b ^= 1;
            }
            std::fs::write(&bad, &d).map_err(|e| e.to_string())?;
            assert!(iureapps_verify(&pubkey, &sig, &bad).is_err());
        }
        // Otra clave no verifica la firma.
        let (other, _) = generate()?;
        let sk = secret_key(&sk_text, "")?;
        let pk = PublicKey::from_secret_key(&sk).map_err(|e| e.to_string())?;
        let sig = sign_file(&pk, &sk, &file)?;
        let other_pub = catalog_pubkey(&secret_key(&other, "")?)?;
        assert!(iureapps_verify(&other_pub, &sig, &file).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn bad_keys_are_errors() {
        assert!(key_text("no es una clave").is_err());
        assert!(key_text(&"A".repeat(MAX_KEY_LEN + 1)).is_err());
        assert!(secret_key("dW50cnVzdGVkIGNvbW1lbnQ6IHgKeHgK", "").is_err());
        assert!(sign(Vec::new()).is_err());
        assert!(keygen(vec!["solo-una".into()]).is_err());
    }

    #[test]
    fn sig_next_to_file() {
        assert_eq!(sig_path(Path::new("dist/a.app.tar.gz")), Path::new("dist/a.app.tar.gz.sig"));
    }

    /// La clave pública publicada en `docs/minisign.pub` es válida.
    #[test]
    fn published_public_key_parses() -> Res {
        let text = std::fs::read_to_string(crate::root().join("docs/minisign.pub")).map_err(|e| e.to_string())?;
        parse_public(&text).map(|_| ())
    }
}
