//! Reparación acotada de JSON devuelto por un modelo: bloque de código Markdown, texto antes o
//! después, comas finales, comillas tipográficas como delimitadores y respuesta cortada (se
//! cierran cadenas, arreglos y objetos). Si aun así no es JSON, se rechaza.

use serde_json::Value;

/// Tope de la entrada.
const MAX_INPUT: usize = 4 * 1024 * 1024;
/// Profundidad máxima de anidación aceptada.
const MAX_DEPTH: usize = 64;

/// Resultado del análisis.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub value: Value,
    /// Hubo que reparar algo.
    pub repaired: bool,
}

/// Analiza el JSON de una respuesta; `Err` con el motivo si no se pudo.
pub fn parse_lenient(raw: &str) -> Result<Parsed, String> {
    if raw.len() > MAX_INPUT {
        return Err("respuesta demasiado grande".into());
    }
    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return check_depth(value, false);
    }
    let candidate = extract(trimmed).ok_or_else(|| "no hay ningún objeto JSON en la respuesta".to_string())?;
    if let Ok(value) = serde_json::from_str::<Value>(candidate) {
        return check_depth(value, true);
    }
    let fixed = repair(candidate)?;
    serde_json::from_str::<Value>(&fixed)
        .map_err(|e| format!("JSON no reparable: {e}"))
        .and_then(|v| check_depth(v, true))
}

fn check_depth(value: Value, repaired: bool) -> Result<Parsed, String> {
    fn depth(v: &Value, d: usize) -> usize {
        if d > MAX_DEPTH {
            return d;
        }
        match v {
            Value::Array(a) => a.iter().map(|x| depth(x, d.saturating_add(1))).max().unwrap_or(d),
            Value::Object(o) => o.values().map(|x| depth(x, d.saturating_add(1))).max().unwrap_or(d),
            _ => d,
        }
    }
    if depth(&value, 0) > MAX_DEPTH {
        return Err("JSON demasiado anidado".into());
    }
    Ok(Parsed { value, repaired })
}

/// Recorta al primer `{` (o `[`) y, si está, a un bloque ```json …```.
fn extract(s: &str) -> Option<&str> {
    let s = match s.find("```") {
        Some(a) => {
            let rest = s.get(a.saturating_add(3)..)?;
            let rest = rest.strip_prefix("json").or_else(|| rest.strip_prefix("JSON")).unwrap_or(rest);
            match rest.find("```") {
                Some(b) => rest.get(..b)?,
                None => rest,
            }
        }
        None => s,
    };
    let start = s.find(['{', '['])?;
    let body = s.get(start..)?;
    // Si hay un cierre que equilibra, se corta ahí (texto después del JSON).
    Some(balanced_prefix(body).unwrap_or(body))
}

/// Prefijo equilibrado (respetando cadenas), si existe.
fn balanced_prefix(s: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escape = false;
    for (i, c) in s.char_indices() {
        if in_str {
            match (escape, c) {
                (true, _) => escape = false,
                (false, '\\') => escape = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' | '[' => depth = depth.saturating_add(1),
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return s.get(..i.saturating_add(c.len_utf8()));
                }
            }
            _ => {}
        }
    }
    None
}

/// Comas finales fuera, comillas tipográficas como delimitadores, y cierre de lo abierto.
fn repair(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len().saturating_add(16));
    let mut stack: Vec<char> = Vec::new();
    let mut in_str = false;
    let mut escape = false;
    for c in s.chars() {
        if in_str {
            match (escape, c) {
                (true, _) => {
                    escape = false;
                    out.push(c);
                }
                (false, '\\') => {
                    escape = true;
                    out.push(c);
                }
                (false, '"') => {
                    in_str = false;
                    out.push(c);
                }
                (false, '\n') => out.push_str("\\n"),
                (false, '\r') => {}
                (false, '\t') => out.push_str("\\t"),
                _ => out.push(c),
            }
            continue;
        }
        match c {
            '"' | '“' | '”' => {
                in_str = true;
                out.push('"');
            }
            '{' => {
                stack.push('}');
                out.push(c);
            }
            '[' => {
                stack.push(']');
                out.push(c);
            }
            '}' | ']' => {
                strip_trailing_comma(&mut out);
                if stack.pop() != Some(c) {
                    return Err("llaves desequilibradas".into());
                }
                out.push(c);
            }
            _ => out.push(c),
        }
        if stack.len() > MAX_DEPTH {
            return Err("JSON demasiado anidado".into());
        }
    }
    if escape {
        out.pop();
    }
    if in_str {
        out.push('"');
    }
    // Respuesta cortada: quita un par clave/valor incompleto final y cierra.
    let trimmed_len = out.trim_end().len();
    out.truncate(trimmed_len);
    if out.ends_with(':') {
        out.push_str(" null");
    }
    strip_trailing_comma(&mut out);
    while let Some(close) = stack.pop() {
        strip_trailing_comma(&mut out);
        out.push(close);
    }
    Ok(out)
}

fn strip_trailing_comma(out: &mut String) {
    let len = out.trim_end().len();
    out.truncate(len);
    if out.ends_with(',') {
        out.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn plain_json_is_not_repaired() {
        let p = parse_lenient(r#" {"a": 1} "#).unwrap();
        assert_eq!(p.value, json!({"a": 1}));
        assert!(!p.repaired);
    }

    #[test]
    fn fenced_and_chatty_output() {
        let raw = "Claro, aquí está:\n```json\n{\"summary\": \"ok\", \"items\": [1, 2,],}\n```\nEspero que sirva.";
        let p = parse_lenient(raw).unwrap();
        assert_eq!(p.value, json!({"summary": "ok", "items": [1, 2]}));
        assert!(p.repaired);
        let p = parse_lenient("Resultado: {\"a\": {\"b\": \"}\"}} y nada más {x}").unwrap();
        assert_eq!(p.value, json!({"a": {"b": "}"}}));
    }

    #[test]
    fn truncated_output_is_closed() {
        let p = parse_lenient("{\"summary\": \"El cliente pide\", \"points\": [\"uno\", \"do").unwrap();
        assert_eq!(p.value, json!({"summary": "El cliente pide", "points": ["uno", "do"]}));
        let p = parse_lenient("{\"a\": 1, \"b\":").unwrap();
        assert_eq!(p.value, json!({"a": 1, "b": null}));
        let p = parse_lenient("{\"a\": \"línea\nnueva\"}").unwrap();
        assert_eq!(p.value, json!({"a": "línea\nnueva"}));
    }

    #[test]
    fn hopeless_output_is_rejected() {
        assert!(parse_lenient("no tengo idea").is_err());
        assert!(parse_lenient("{\"a\": ]").is_err());
        assert!(parse_lenient("{\"a\": nope}").is_err());
        let deep = "[".repeat(200);
        assert!(parse_lenient(&deep).is_err());
    }

    proptest! {
        #[test]
        fn never_panics(s in "\\PC{0,300}") {
            let _ = parse_lenient(&s);
        }

        #[test]
        fn valid_json_survives_truncation_or_is_rejected(n in 0usize..60) {
            let full = r#"{"title": "Minuta", "items": [{"t": "a", "d": "2026-10-27"}, {"t": "b"}], "ok": true}"#;
            let cut: String = full.chars().take(n).collect();
            if let Ok(p) = parse_lenient(&cut) {
                prop_assert!(p.value.is_object() || p.value.is_array());
            }
        }
    }
}
