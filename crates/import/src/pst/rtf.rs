//! Cuerpos RTF de Outlook (`PR_RTF_COMPRESSED`): descompresión LZFu ([MS-OXRTFCP]) y extracción de
//! texto o del HTML encapsulado ([MS-OXRTFEX], de forma simplificada).
//!
//! Solo se usa cuando el mensaje no trae `PR_BODY` ni `PR_HTML` (raro: Outlook suele guardar los
//! tres).

/// Diccionario inicial de LZFu.
const PREBUF: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";

const LZFU: u32 = 0x7546_5A4C;
const MELA: u32 = 0x414C_454D;

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// Descomprime `PR_RTF_COMPRESSED`. `max` acota la salida.
pub fn decompress(data: &[u8], max: usize) -> Option<Vec<u8>> {
    let raw_size = usize::try_from(le32(data, 4)?).ok()?;
    let kind = le32(data, 8)?;
    let body = data.get(16..)?;
    let limit = raw_size.min(max);
    match kind {
        MELA => Some(body.get(..limit.min(body.len()))?.to_vec()),
        LZFU => {
            let mut dict = [0u8; 4096];
            dict.get_mut(..PREBUF.len())?.copy_from_slice(PREBUF);
            let mut wpos = PREBUF.len();
            let mut out = Vec::with_capacity(limit.min(1 << 20));
            let mut i = 0usize;
            'outer: while i < body.len() && out.len() < limit {
                let control = *body.get(i)?;
                i = i.saturating_add(1);
                for bit in 0..8 {
                    if i >= body.len() || out.len() >= limit {
                        break 'outer;
                    }
                    if control & (1 << bit) == 0 {
                        let b = *body.get(i)?;
                        i = i.saturating_add(1);
                        out.push(b);
                        *dict.get_mut(wpos)? = b;
                        wpos = (wpos + 1) % 4096;
                    } else {
                        let hi = usize::from(*body.get(i)?);
                        let lo = usize::from(*body.get(i.saturating_add(1))?);
                        i = i.saturating_add(2);
                        let word = (hi << 8) | lo;
                        let offset = word >> 4;
                        let len = (word & 0x0F) + 2;
                        if offset == wpos {
                            break 'outer;
                        }
                        for k in 0..len {
                            let b = *dict.get((offset + k) % 4096)?;
                            out.push(b);
                            *dict.get_mut(wpos)? = b;
                            wpos = (wpos + 1) % 4096;
                            if out.len() >= limit {
                                break 'outer;
                            }
                        }
                    }
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// Destinos RTF cuyo contenido no es texto del cuerpo.
const SKIP_DESTS: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "pict",
    "object",
    "header",
    "footer",
    "listtable",
    "listoverridetable",
    "rsidtbl",
    "generator",
    "themedata",
    "datastore",
    "latentstyles",
    "xmlnstbl",
    "mmathPr",
];

/// Texto del cuerpo de un RTF. Si encapsula HTML (`\fromhtml1`), devuelve el HTML y `true`.
pub fn extract(rtf: &[u8], max: usize) -> (String, bool) {
    let is_html = rtf.windows(10).take(4096).any(|w| w == b"\\fromhtml1");
    let mut out: Vec<u8> = Vec::new();
    // Por grupo: (saltar, dentro de htmltag).
    let mut stack: Vec<(bool, bool)> = vec![(false, false)];
    let mut htmlrtf = false;
    let mut uc_skip = 1usize;
    let mut pending_skip = 0usize;
    let mut i = 0usize;
    let push_char = |out: &mut Vec<u8>, c: char| {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    };
    while i < rtf.len() && out.len() < max {
        let (skip, in_tag) = stack.last().copied().unwrap_or((false, false));
        let visible = !skip && (!is_html || in_tag || !htmlrtf);
        let Some(&b) = rtf.get(i) else { break };
        match b {
            b'{' => {
                if stack.len() < 512 {
                    stack.push((skip, in_tag));
                }
                i += 1;
            }
            b'}' => {
                if stack.len() > 1 {
                    stack.pop();
                }
                i += 1;
            }
            b'\\' => {
                let next = rtf.get(i + 1).copied().unwrap_or(0);
                if next.is_ascii_alphabetic() {
                    let start = i + 1;
                    let mut j = start;
                    while rtf.get(j).is_some_and(u8::is_ascii_alphabetic) && j - start < 32 {
                        j += 1;
                    }
                    let word = String::from_utf8_lossy(rtf.get(start..j).unwrap_or(&[])).into_owned();
                    let num_start = j;
                    if rtf.get(j) == Some(&b'-') {
                        j += 1;
                    }
                    while rtf.get(j).is_some_and(u8::is_ascii_digit) && j - num_start < 11 {
                        j += 1;
                    }
                    let param: Option<i64> =
                        std::str::from_utf8(rtf.get(num_start..j).unwrap_or(&[])).ok().and_then(|s| s.parse().ok());
                    if rtf.get(j) == Some(&b' ') {
                        j += 1;
                    }
                    i = j;
                    // ¿Primera palabra de un grupo `{\*\destino`?
                    let at_group_start = stack.len() > 1;
                    match word.as_str() {
                        "htmltag" if is_html => {
                            if let Some(top) = stack.last_mut() {
                                *top = (false, true);
                            }
                        }
                        "htmlrtf" => htmlrtf = param != Some(0),
                        "par" | "line" if visible && (!is_html || in_tag || !htmlrtf) => {
                            if !is_html || !in_tag {
                                out.extend_from_slice(b"\r\n");
                            }
                        }
                        "tab" if visible => out.push(b'\t'),
                        "uc" => uc_skip = usize::try_from(param.unwrap_or(1).clamp(0, 10)).unwrap_or(1),
                        "u" => {
                            if visible {
                                let v = param.unwrap_or(0);
                                let v = if v < 0 { v + 65_536 } else { v };
                                push_char(
                                    &mut out,
                                    u32::try_from(v).ok().and_then(char::from_u32).unwrap_or('\u{fffd}'),
                                );
                            }
                            pending_skip = uc_skip;
                        }
                        w if at_group_start && SKIP_DESTS.contains(&w) => {
                            if let Some(top) = stack.last_mut() {
                                top.0 = true;
                            }
                        }
                        _ => {}
                    }
                    continue;
                }
                match next {
                    b'\'' => {
                        let hex = rtf.get(i + 2..i + 4).and_then(|h| std::str::from_utf8(h).ok());
                        if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                            if pending_skip > 0 {
                                pending_skip -= 1;
                            } else if visible {
                                out.extend_from_slice(crate::util::decode_cp1252(&[v]).as_bytes());
                            }
                        }
                        i += 4;
                    }
                    b'*' => {
                        // Destino opcional: se salta salvo que sea `\htmltag` (lo decide la palabra siguiente).
                        let rest = rtf.get(i + 2..(i + 12).min(rtf.len())).unwrap_or(&[]);
                        let is_tag = rest.starts_with(b"\\htmltag") && is_html;
                        if !is_tag && let Some(top) = stack.last_mut() {
                            top.0 = true;
                        }
                        i += 2;
                    }
                    b'\\' | b'{' | b'}' => {
                        if visible {
                            out.push(next);
                        }
                        i += 2;
                    }
                    b'~' => {
                        if visible {
                            out.push(b' ');
                        }
                        i += 2;
                    }
                    b'\r' | b'\n' => {
                        if visible && !is_html {
                            out.extend_from_slice(b"\r\n");
                        }
                        i += 2;
                    }
                    _ => i += 2,
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                if pending_skip > 0 {
                    pending_skip -= 1;
                } else if visible {
                    out.push(b);
                }
                i += 1;
            }
        }
    }
    (String::from_utf8_lossy(&out).into_owned(), is_html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decompresses_spec_example() {
        // Ejemplo de [MS-OXRTFCP] 3.1.1 («{\rtf1\ansi\ansicpg1252\pard hello world}\r\n»).
        let data: [u8; 61] = [
            0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5, 0xc7, 0xa7, 0x03, 0x00,
            0x0a, 0x00, 0x72, 0x63, 0x70, 0x67, 0x31, 0x32, 0x35, 0x42, 0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65, 0x6c, 0x09,
            0x00, 0x20, 0x62, 0x77, 0x05, 0xb0, 0x6c, 0x64, 0x7d, 0x0a, 0x80, 0x0f, 0xa0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0,
        ];
        let out = decompress(&data, 1 << 20).unwrap();
        assert_eq!(out, b"{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n");
        let (text, html) = extract(&out, 1 << 20);
        assert_eq!(text.trim(), "hello world");
        assert!(!html);
    }

    #[test]
    fn uncompressed_and_garbage() {
        let mut d = vec![0u8; 16];
        d[4] = 5;
        d[8..12].copy_from_slice(&MELA.to_le_bytes());
        d.extend_from_slice(b"{\\rtf1 hola}");
        assert_eq!(decompress(&d, 100).unwrap(), b"{\\rtf");
        assert!(decompress(b"corto", 100).is_none());
    }

    #[test]
    fn plain_rtf_text() {
        let rtf = br"{\rtf1\ansi{\fonttbl{\f0 Arial;}}{\colortbl;\red0\green0\blue0;}\pard Hola\par Se\'f1or \u8364?uro\tab x}";
        let (t, html) = extract(rtf, 1 << 20);
        assert!(!html);
        assert_eq!(t, "Hola\r\nSeñor €uro\tx");
    }

    #[test]
    fn encapsulated_html() {
        let rtf = br"{\rtf1\ansi\fromhtml1 {\*\htmltag19 <html>}{\*\htmltag50 <body>}\htmlrtf {\htmlrtf0 Hola {\*\htmltag84 <b>}mundo{\*\htmltag92 </b>}{\*\htmltag58 </body></html>}}";
        let (t, html) = extract(rtf, 1 << 20);
        assert!(html);
        assert!(t.contains("<html>") && t.contains("<b>mundo</b>"), "{t}");
        assert!(t.contains("Hola"), "{t}");
    }

    proptest::proptest! {
        #[test]
        fn never_panics(b in proptest::collection::vec(proptest::num::u8::ANY, 0..2000)) {
            let _ = decompress(&b, 10_000);
            let (t, _) = extract(&b, 10_000);
            proptest::prop_assert!(t.len() <= 10_000 + 8);
        }
    }
}
