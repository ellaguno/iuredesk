//! Cifrado de Kerberos: `aes256-cts-hmac-sha1-96` (18) y `aes128-cts-hmac-sha1-96` (17), los de
//! Active Directory desde 2008 (RFC 3961 y RFC 3962). RC4-HMAC (23) no: Microsoft lo retira y se
//! desactiva en los dominios actuales.
//!
//! - `string2key`: PBKDF2-HMAC-SHA1 (4096 vueltas por defecto) y `DK(clave, "kerberos")`;
//! - claves por uso: `Kc`/`Ke`/`Ki` = `DK(base, uso ‖ 0x99/0xAA/0x55)`;
//! - cifrar: confusor aleatorio de 16 bytes, AES-CBC con robo de texto cifrado (CTS) e IV cero,
//!   y HMAC-SHA1 truncado a 96 bits del texto claro.

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use super::KerberosError;

/// `aes128-cts-hmac-sha1-96`.
pub const AES128: i32 = 17;
/// `aes256-cts-hmac-sha1-96`.
pub const AES256: i32 = 18;
/// Tipo de checksum `hmac-sha1-96-aes128`.
pub const CKSUM_AES128: i32 = 15;
/// Tipo de checksum `hmac-sha1-96-aes256`.
pub const CKSUM_AES256: i32 = 16;
/// Vueltas de PBKDF2 por defecto.
pub const DEFAULT_ITERATIONS: u32 = 4096;
/// Tope de vueltas que acepta el cliente (un KDC hostil podría pedir millones).
const MAX_ITERATIONS: u32 = 1_000_000;

const BLOCK: usize = 16;
const MAC: usize = 12;

/// Una clave de Kerberos (tipo de cifrado y bytes).
#[derive(Clone, PartialEq, Eq)]
pub struct Key {
    pub etype: i32,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Key").field("etype", &self.etype).finish_non_exhaustive()
    }
}

/// Longitud de la clave de un tipo de cifrado admitido.
pub fn key_len(etype: i32) -> Option<usize> {
    match etype {
        AES128 => Some(16),
        AES256 => Some(32),
        _ => None,
    }
}

/// Tipo de checksum que va con un tipo de cifrado.
pub fn checksum_type(etype: i32) -> i32 {
    if etype == AES128 { CKSUM_AES128 } else { CKSUM_AES256 }
}

fn unsupported(etype: i32) -> KerberosError {
    KerberosError::Protocol(format!("tipo de cifrado no admitido: {etype}"))
}

/// n-fold de RFC 3961 §5.1: repite la entrada rotándola 13 bits cada vez y suma en
/// complemento a uno hasta `n_bytes`.
pub fn nfold(input: &[u8], n_bytes: usize) -> Vec<u8> {
    let k = input.len();
    if k == 0 || n_bytes == 0 {
        return vec![0; n_bytes];
    }
    let lcm = num_lcm(k, n_bytes);
    // Concatenación de las copias rotadas (13 bits a la derecha cada vez), en bits.
    let total_bits = lcm * 8;
    let in_bits = k * 8;
    let bit_at = |i: usize| -> u8 {
        // Bit `i` de la tira rotada: copia `c` rotada 13·c bits a la derecha.
        let c = i / in_bits;
        let pos = i % in_bits;
        let rot = (13 * c) % in_bits;
        let src = (pos + in_bits - rot) % in_bits;
        let byte = input.get(src / 8).copied().unwrap_or(0);
        (byte >> (7 - (src % 8))) & 1
    };
    let mut stream = vec![0u8; lcm];
    for i in 0..total_bits {
        if bit_at(i) == 1
            && let Some(b) = stream.get_mut(i / 8)
        {
            *b |= 0x80 >> (i % 8);
        }
    }
    // Suma en complemento a uno de los trozos de n bytes.
    let mut acc = vec![0u32; n_bytes];
    for chunk in stream.chunks(n_bytes) {
        for (a, b) in acc.iter_mut().zip(chunk) {
            *a += u32::from(*b);
        }
    }
    loop {
        let mut carry = 0u32;
        for a in acc.iter_mut().rev() {
            *a += carry;
            carry = *a >> 8;
            *a &= 0xFF;
        }
        if carry == 0 {
            break;
        }
        if let Some(last) = acc.last_mut() {
            *last += carry;
        }
    }
    acc.into_iter().map(|a| a as u8).collect()
}

fn num_lcm(a: usize, b: usize) -> usize {
    fn gcd(mut a: usize, mut b: usize) -> usize {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    }
    a / gcd(a, b) * b
}

/// AES de un bloque (cifrar).
fn ecb_encrypt(key: &[u8], block: &[u8; BLOCK]) -> Result<[u8; BLOCK], KerberosError> {
    let mut b = GenericArray::clone_from_slice(block);
    match key.len() {
        16 => aes::Aes128::new_from_slice(key).map_err(|_| unsupported(0))?.encrypt_block(&mut b),
        32 => aes::Aes256::new_from_slice(key).map_err(|_| unsupported(0))?.encrypt_block(&mut b),
        _ => return Err(unsupported(0)),
    }
    Ok(b.into())
}

/// AES de un bloque (descifrar).
fn ecb_decrypt(key: &[u8], block: &[u8; BLOCK]) -> Result<[u8; BLOCK], KerberosError> {
    let mut b = GenericArray::clone_from_slice(block);
    match key.len() {
        16 => aes::Aes128::new_from_slice(key).map_err(|_| unsupported(0))?.decrypt_block(&mut b),
        32 => aes::Aes256::new_from_slice(key).map_err(|_| unsupported(0))?.decrypt_block(&mut b),
        _ => return Err(unsupported(0)),
    }
    Ok(b.into())
}

fn xor(a: &[u8; BLOCK], b: &[u8]) -> [u8; BLOCK] {
    let mut out = *a;
    for (o, x) in out.iter_mut().zip(b) {
        *o ^= x;
    }
    out
}

fn block_at(data: &[u8], i: usize) -> [u8; BLOCK] {
    let mut b = [0u8; BLOCK];
    if let Some(src) = data.get(i * BLOCK..) {
        let n = src.len().min(BLOCK);
        if let (Some(dst), Some(s)) = (b.get_mut(..n), src.get(..n)) {
            dst.copy_from_slice(s);
        }
    }
    b
}

/// AES-CBC con robo de texto cifrado (CS3: los dos últimos bloques siempre se cruzan), IV dado.
pub fn cts_encrypt(key: &[u8], iv: &[u8; BLOCK], plain: &[u8]) -> Result<Vec<u8>, KerberosError> {
    let n = plain.len();
    if n < BLOCK {
        return Err(KerberosError::Protocol("CTS: menos de un bloque".into()));
    }
    let m = n.div_ceil(BLOCK);
    let mut prev = *iv;
    let mut blocks: Vec<[u8; BLOCK]> = Vec::with_capacity(m);
    for i in 0..m {
        let c = ecb_encrypt(key, &xor(&prev, &block_at(plain, i)))?;
        blocks.push(c);
        prev = c;
    }
    if m == 1 {
        return Ok(blocks.concat());
    }
    let last_len = n - BLOCK * (m - 1);
    let mut out = Vec::with_capacity(n);
    for b in blocks.iter().take(m - 2) {
        out.extend_from_slice(b);
    }
    out.extend_from_slice(blocks.get(m - 1).map(|b| &b[..]).unwrap_or_default());
    out.extend_from_slice(blocks.get(m - 2).and_then(|b| b.get(..last_len)).unwrap_or_default());
    Ok(out)
}

/// Inverso de [`cts_encrypt`].
pub fn cts_decrypt(key: &[u8], iv: &[u8; BLOCK], data: &[u8]) -> Result<Vec<u8>, KerberosError> {
    let n = data.len();
    if n < BLOCK {
        return Err(KerberosError::Protocol("CTS: menos de un bloque".into()));
    }
    let m = n.div_ceil(BLOCK);
    if m == 1 {
        return Ok(xor(&ecb_decrypt(key, &block_at(data, 0))?, iv).to_vec());
    }
    let last_len = n - BLOCK * (m - 1);
    let mut out = Vec::with_capacity(n);
    let mut prev = *iv;
    for i in 0..m - 2 {
        let c = block_at(data, i);
        out.extend_from_slice(&xor(&ecb_decrypt(key, &c)?, &prev));
        prev = c;
    }
    // Bloque m-1 en la posición m-2 (completo) y el resto de C(m-1) al final (parcial).
    let cn = block_at(data, m - 2);
    let tail = data.get(BLOCK * (m - 1)..).unwrap_or_default();
    let d = ecb_decrypt(key, &cn)?;
    let mut c_prev = d;
    if let Some(head) = c_prev.get_mut(..last_len) {
        head.copy_from_slice(tail);
    }
    let last = xor(&d, &c_prev);
    out.extend_from_slice(&xor(&ecb_decrypt(key, &c_prev)?, &prev));
    out.extend_from_slice(last.get(..last_len).unwrap_or_default());
    Ok(out)
}

/// DR/DK de RFC 3961 §5.1 (para AES, `random-to-key` es la identidad).
pub fn derive(base: &[u8], constant: &[u8]) -> Result<Vec<u8>, KerberosError> {
    let folded = nfold(constant, BLOCK);
    let mut block = [0u8; BLOCK];
    block.copy_from_slice(&folded);
    let mut out = Vec::with_capacity(base.len());
    while out.len() < base.len() {
        block = ecb_encrypt(base, &block)?;
        out.extend_from_slice(&block);
    }
    out.truncate(base.len());
    Ok(out)
}

/// Clave a partir de la contraseña (RFC 3962 §4). `params` es `s2kparams` (vueltas en big
/// endian), si el KDC lo mandó.
pub fn string_to_key(etype: i32, password: &str, salt: &[u8], params: Option<&[u8]>) -> Result<Key, KerberosError> {
    let len = key_len(etype).ok_or_else(|| unsupported(etype))?;
    let iterations = match params {
        Some(p) if p.len() == 4 => u32::from_be_bytes([p[0], p[1], p[2], p[3]]),
        Some(_) => return Err(KerberosError::Protocol("s2kparams no válido".into())),
        None => DEFAULT_ITERATIONS,
    };
    if iterations == 0 || iterations > MAX_ITERATIONS {
        return Err(KerberosError::Protocol(format!("vueltas de PBKDF2 fuera de rango: {iterations}")));
    }
    let mut tkey = vec![0u8; len];
    pbkdf2::pbkdf2_hmac::<Sha1>(password.as_bytes(), salt, iterations, &mut tkey);
    Ok(Key { etype, bytes: derive(&tkey, b"kerberos")? })
}

fn usage_key(key: &Key, usage: u32, tag: u8) -> Result<Vec<u8>, KerberosError> {
    let mut c = usage.to_be_bytes().to_vec();
    c.push(tag);
    derive(&key.bytes, &c)
}

fn hmac96(key: &[u8], data: &[u8]) -> Result<[u8; MAC], KerberosError> {
    let mut m = <Hmac<Sha1> as Mac>::new_from_slice(key).map_err(|_| unsupported(0))?;
    m.update(data);
    let full = m.finalize().into_bytes();
    let mut out = [0u8; MAC];
    out.copy_from_slice(full.get(..MAC).unwrap_or(&[0; MAC]));
    Ok(out)
}

/// Cifra `plain` con la clave para el uso dado; `confounder` son 16 bytes aleatorios.
pub fn encrypt_with(key: &Key, usage: u32, plain: &[u8], confounder: [u8; BLOCK]) -> Result<Vec<u8>, KerberosError> {
    key_len(key.etype).ok_or_else(|| unsupported(key.etype))?;
    let ke = usage_key(key, usage, 0xAA)?;
    let ki = usage_key(key, usage, 0x55)?;
    let mut data = confounder.to_vec();
    data.extend_from_slice(plain);
    let mut out = cts_encrypt(&ke, &[0; BLOCK], &data)?;
    out.extend_from_slice(&hmac96(&ki, &data)?);
    Ok(out)
}

/// [`encrypt_with`] con un confusor aleatorio.
pub fn encrypt(key: &Key, usage: u32, plain: &[u8]) -> Result<Vec<u8>, KerberosError> {
    let mut c = [0u8; BLOCK];
    getrandom::getrandom(&mut c).map_err(|e| KerberosError::Protocol(e.to_string()))?;
    encrypt_with(key, usage, plain, c)
}

/// Descifra y comprueba la integridad. Un HMAC que no cuadra es [`KerberosError::Integrity`]
/// (clave equivocada: contraseña distinta o mensaje alterado).
pub fn decrypt(key: &Key, usage: u32, data: &[u8]) -> Result<Vec<u8>, KerberosError> {
    key_len(key.etype).ok_or_else(|| unsupported(key.etype))?;
    if data.len() < BLOCK + MAC {
        return Err(KerberosError::Protocol("cifrado demasiado corto".into()));
    }
    let (cipher, mac) = data.split_at(data.len() - MAC);
    let ke = usage_key(key, usage, 0xAA)?;
    let ki = usage_key(key, usage, 0x55)?;
    let plain = cts_decrypt(&ke, &[0; BLOCK], cipher)?;
    let expected = hmac96(&ki, &plain)?;
    // Comparación sin atajos.
    if expected.iter().zip(mac).fold(0u8, |acc, (a, b)| acc | (a ^ b)) != 0 {
        return Err(KerberosError::Integrity);
    }
    Ok(plain.get(BLOCK..).unwrap_or_default().to_vec())
}

/// Checksum con clave (`hmac-sha1-96-aes*`) para el uso dado.
pub fn checksum(key: &Key, usage: u32, data: &[u8]) -> Result<Vec<u8>, KerberosError> {
    let kc = usage_key(key, usage, 0x99)?;
    Ok(hmac96(&kc, data)?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn round_trip_every_length() {
        for etype in [AES128, AES256] {
            let key = Key { etype, bytes: (0..key_len(etype).unwrap() as u8).collect() };
            for len in [0usize, 1, 15, 16, 17, 31, 32, 33, 100] {
                let plain: Vec<u8> = (0..len as u8).collect();
                let c = encrypt(&key, 3, &plain).unwrap();
                assert_eq!(c.len(), len + 16 + 12);
                assert_eq!(decrypt(&key, 3, &c).unwrap(), plain);
                // Otro uso u otra clave: falla la integridad, no da basura.
                assert_eq!(decrypt(&key, 4, &c), Err(KerberosError::Integrity));
                let mut bad = c.clone();
                bad[0] ^= 1;
                assert_eq!(decrypt(&key, 3, &bad), Err(KerberosError::Integrity));
            }
        }
        let k = Key { etype: AES256, bytes: vec![0; 32] };
        assert!(decrypt(&k, 3, &[0; 27]).is_err());
        assert!(decrypt(&Key { etype: 23, bytes: vec![0; 16] }, 3, &[0; 40]).is_err());
    }

    #[test]
    fn hostile_iterations_are_refused() {
        assert!(string_to_key(AES256, "p", b"s", Some(&[0xFF, 0xFF, 0xFF, 0xFF])).is_err());
        assert!(string_to_key(AES256, "p", b"s", Some(&[0, 0, 0, 0])).is_err());
        assert!(string_to_key(AES256, "p", b"s", Some(&[1, 2])).is_err());
        assert!(string_to_key(23, "p", b"s", None).is_err());
    }

    /// RFC 3961 A.1.
    #[test]
    fn nfold_vectors() {
        for (n, input, out) in [
            (64, &b"012345"[..], "be072631276b1955"),
            (56, b"password", "78a07b6caf85fa"),
            (64, b"Rough Consensus, and Running Code", "bb6ed30870b7f0e0"),
            (168, b"password", "59e4a8ca7c0385c3c37b3f6d2000247cb6e6bd5b3e"),
            (192, b"MASSACHVSETTS INSTITVTE OF TECHNOLOGY", "db3b0d8f0b061e603282b308a50841229ad798fab9540c1b"),
            (168, b"Q", "518a54a215a8452a518a54a215a8452a518a54a215"),
            (168, b"ba", "fb25d531ae8974499f52fd92ea9857c4ba24cf297e"),
            (64, b"kerberos", "6b65726265726f73"),
            (128, b"kerberos", "6b65726265726f737b9b5b2b93132b93"),
            (168, b"kerberos", "8372c236344e5f1550cd0747e15d62ca7a5a3bcea4"),
            (256, b"kerberos", "6b65726265726f737b9b5b2b93132b935c9bdcdad95c9899c4cae4dee6d6cae4"),
        ] {
            assert_eq!(nfold(input, n / 8), hex(out), "{n}-fold {input:?}");
        }
    }

    /// RFC 3962 apéndice B: string-to-key (PBKDF2 + DK «kerberos»).
    #[test]
    fn string_to_key_vectors() {
        let x64 = "X".repeat(64);
        let x65 = "X".repeat(65);
        let clef = String::from_utf8(vec![0xf0, 0x9d, 0x84, 0x9e]).unwrap();
        for (iter, pass, salt, k128, k256) in [
            (
                1u32,
                "password",
                &b"ATHENA.MIT.EDUraeburn"[..],
                "42263c6e89f4fc28b8df68ee09799f15",
                "fe697b52bc0d3ce14432ba036a92e65bbb52280990a2fa27883998d72af30161",
            ),
            (
                2,
                "password",
                b"ATHENA.MIT.EDUraeburn",
                "c651bf29e2300ac27fa469d693bdda13",
                "a2e16d16b36069c135d5e9d2e25f896102685618b95914b467c67622225824ff",
            ),
            (
                1200,
                "password",
                b"ATHENA.MIT.EDUraeburn",
                "4c01cd46d632d01e6dbe230a01ed642a",
                "55a6ac740ad17b4846941051e1e8b0a7548d93b0ab30a8bc3ff16280382b8c2a",
            ),
            (
                5,
                "password",
                &[0x12, 0x34, 0x56, 0x78, 0x78, 0x56, 0x34, 0x12],
                "e9b23d52273747dd5c35cb55be619d8e",
                "97a4e786be20d81a382d5ebc96d5909cabcdadc87ca48f574504159f16c36e31",
            ),
            (
                1200,
                x64.as_str(),
                b"pass phrase equals block size",
                "59d1bb789a828b1aa54ef9c2883f69ed",
                "89adee3608db8bc71f1bfbfe459486b05618b70cbae22092534e56c553ba4b34",
            ),
            (
                1200,
                x65.as_str(),
                b"pass phrase exceeds block size",
                "cb8005dc5f90179a7f02104c0018751d",
                "d78c5c9cb872a8c9dad4697f0bb5b2d21496c82beb2caeda2112fceea057401b",
            ),
            (
                50,
                clef.as_str(),
                b"EXAMPLE.COMpianist",
                "f149c1f2e154a73452d43e7fe62a56e5",
                "4b6d9839f84406df1f09cc166db4b83c571848b784a3d6bdc346589a3e393f9e",
            ),
        ] {
            let p = iter.to_be_bytes();
            assert_eq!(string_to_key(AES128, pass, salt, Some(&p)).unwrap().bytes, hex(k128), "aes128 {iter}");
            assert_eq!(string_to_key(AES256, pass, salt, Some(&p)).unwrap().bytes, hex(k256), "aes256 {iter}");
        }
    }

    /// RFC 3962 apéndice B: AES-CTS con IV cero.
    #[test]
    fn cts_vectors() {
        let key = hex("636869636b656e207465726979616b69");
        let full = hex("4920776f756c64206c696b65207468652047656e6572616c2047617527732043686963
                        6b656e2c20706c656173652c20616e6420776f6e746f6e20736f75702e");
        for (len, out) in [
            (17, "c6353568f2bf8cb4d8a580362da7ff7f97"),
            (31, "fc00783e0efdb2c1d445d4c8eff7ed2297687268d6ecccc0c07b25e25ecfe5"),
            (32, "39312523a78662d5be7fcbcc98ebf5a897687268d6ecccc0c07b25e25ecfe584"),
            (47, "97687268d6ecccc0c07b25e25ecfe584b3fffd940c16a18c1b5549d2f838029e39312523a78662d5be7fcbcc98ebf5"),
            (48, "97687268d6ecccc0c07b25e25ecfe5849dad8bbb96c4cdc03bc103e1a194bbd839312523a78662d5be7fcbcc98ebf5a8"),
            (
                64,
                "97687268d6ecccc0c07b25e25ecfe58439312523a78662d5be7fcbcc98ebf5a84807efe836ee89a526730dbc2f7bc8409dad8bbb96c4cdc03bc103e1a194bbd8",
            ),
        ] {
            let plain = &full[..len];
            assert_eq!(cts_encrypt(&key, &[0; 16], plain).unwrap(), hex(out), "cifrar {len}");
            assert_eq!(cts_decrypt(&key, &[0; 16], &hex(out)).unwrap(), plain, "descifrar {len}");
        }
    }
}
