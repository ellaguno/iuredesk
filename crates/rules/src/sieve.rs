//! Reglas ⇄ Sieve (RFC 5228, con `fileinto`, `imap4flags` RFC 5232, `copy` RFC 3894,
//! `vacation` RFC 5230, `regex`, `body` RFC 5173, `date`/`relational` RFC 5260/5231).
//!
//! [`to_sieve`] genera el script que se sube por ManageSieve con lo que el servidor sabe hacer y
//! dice qué reglas se quedan en local (y por qué). [`from_sieve`] lee un script (el nuestro u otro
//! escrito a mano) y devuelve las reglas que entiende; lo demás se cuenta como omitido.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use iuredesk_model::AccountId;

use crate::Bi;
use crate::error::{Result, RulesError};
use crate::rule::{Action, Condition, Field, FolderRef, Rule, TextOp};
use crate::vacation::Vacation;

/// Tamaño máximo de un script.
pub const MAX_SCRIPT: usize = 1024 * 1024;
/// Anidamiento máximo de bloques y pruebas.
const MAX_DEPTH: usize = 32;
/// Piezas léxicas como mucho.
const MAX_TOKENS: usize = 200_000;
/// Nombre del script en el servidor.
pub const SCRIPT_NAME: &str = "iuredesk";
/// Marca de los comentarios con metadatos.
const META: &str = "iuredesk:";

// ------------------------------------------------------------------------------------ generar

/// Contexto para generar.
#[derive(Clone, Debug, Default)]
pub struct GenCtx {
    /// Cuenta del script.
    pub account: AccountId,
    /// Extensiones del servidor (`SIEVE` de la capacidad de ManageSieve), en minúsculas.
    pub extensions: BTreeSet<String>,
    /// Ruta de la papelera de la cuenta.
    pub trash_path: Option<String>,
    /// Mis direcciones (`vacation :addresses`).
    pub my_addresses: Vec<String>,
}

impl GenCtx {
    fn has(&self, ext: &str) -> bool {
        self.extensions.contains(ext)
    }
}

/// Script generado.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Script {
    /// Texto del script.
    pub text: String,
    /// Reglas que corren en el servidor.
    pub server: Vec<u64>,
    /// Reglas que se quedan en local y por qué.
    pub local: Vec<(u64, Bi)>,
    /// La respuesta de vacaciones corre en el servidor.
    pub vacation: bool,
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn quote_list(items: &[String]) -> String {
    if let [one] = items {
        return quote(one);
    }
    format!("[{}]", items.iter().map(|s| quote(s)).collect::<Vec<_>>().join(", "))
}

/// Escapa los comodines de `:matches`.
fn wild_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '*' || c == '?' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn header_names(field: &Field) -> Option<Vec<String>> {
    Some(match field {
        Field::From => vec!["from".into()],
        Field::To => vec!["to".into()],
        Field::Cc => vec!["cc".into()],
        Field::Recipients => vec!["to".into(), "cc".into()],
        Field::Subject => vec!["subject".into()],
        Field::ListId => vec!["list-id".into()],
        Field::Header(h) => {
            let h = h.trim().to_ascii_lowercase();
            if h.is_empty() || !h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return None;
            }
            vec![h]
        }
        Field::Body => return None,
    })
}

fn not_expr(why_en: &str, why_es: &str) -> Bi {
    Bi::new(why_en, why_es)
}

fn gen_test(c: &Condition, ctx: &GenCtx, req: &mut BTreeSet<&'static str>) -> std::result::Result<String, Bi> {
    Ok(match c {
        Condition::Text { field: Field::Body, op, value } => {
            if !ctx.has("body") {
                return Err(not_expr("the server can't search the message text", "el servidor no busca en el texto"));
            }
            req.insert("body");
            let (m, v) = match_type(*op, value, ctx, req)?;
            let t = format!("body :text {m} {}", quote(&v));
            if *op == TextOp::NotContains { format!("not {t}") } else { t }
        }
        Condition::Text { field, op, value } => {
            let names = header_names(field).ok_or_else(|| {
                not_expr("that header can't be used on the server", "esa cabecera no se puede usar en el servidor")
            })?;
            let (m, v) = match_type(*op, value, ctx, req)?;
            let t = format!("header {m} {} {}", quote_list(&names), quote(&v));
            if *op == TextOp::NotContains { format!("not {t}") } else { t }
        }
        Condition::Domain { field, domain } => {
            let names = header_names(field)
                .filter(|_| field.is_address())
                .ok_or_else(|| not_expr("only addresses have a domain", "solo las direcciones tienen dominio"))?;
            let d = domain.trim().trim_start_matches('@').to_lowercase();
            let pats = vec![wild_escape(&d), format!("*.{}", wild_escape(&d))];
            format!("address :domain :matches {} {}", quote_list(&names), quote_list(&pats))
        }
        Condition::SizeOver { bytes } => format!("size :over {bytes}"),
        Condition::SizeUnder { bytes } => format!("size :under {bytes}"),
        Condition::IsReply => "exists \"in-reply-to\"".into(),
        Condition::IsList => "exists \"list-id\"".into(),
        Condition::Account { account } => {
            if *account == ctx.account {
                "true".into()
            } else {
                "false".into()
            }
        }
        Condition::HasAttachment => {
            return Err(not_expr("the server can't check attachments", "el servidor no revisa adjuntos"));
        }
        Condition::InFolder { .. } => {
            return Err(not_expr("the server only filters arriving mail", "el servidor solo filtra lo que llega"));
        }
        Condition::KnownContact => {
            return Err(not_expr("the server doesn't know your contacts", "el servidor no conoce tus contactos"));
        }
    })
}

fn match_type(
    op: TextOp,
    value: &str,
    ctx: &GenCtx,
    req: &mut BTreeSet<&'static str>,
) -> std::result::Result<(&'static str, String), Bi> {
    let v = value.trim();
    Ok(match op {
        TextOp::Contains | TextOp::NotContains => (":contains", v.to_string()),
        TextOp::Is => (":is", v.to_string()),
        TextOp::StartsWith => (":matches", format!("{}*", wild_escape(v))),
        TextOp::EndsWith => (":matches", format!("*{}", wild_escape(v))),
        TextOp::Regex => {
            if !ctx.has("regex") {
                return Err(not_expr("the server doesn't support patterns", "el servidor no admite patrones"));
            }
            req.insert("regex");
            (":regex", v.to_string())
        }
    })
}

fn gen_actions(rule: &Rule, ctx: &GenCtx, req: &mut BTreeSet<&'static str>) -> std::result::Result<Vec<String>, Bi> {
    let mut flags = Vec::new();
    let mut tail = Vec::new();
    let mut stop = false;
    for a in &rule.actions {
        match a {
            Action::Flag => flags.push("\\Flagged".to_string()),
            Action::MarkRead => flags.push("\\Seen".to_string()),
            Action::Label { name } => flags.push(crate::apply::imap_keyword(name)),
            Action::MoveTo { folder } => {
                if !ctx.has("fileinto") {
                    return Err(not_expr("the server can't file into folders", "el servidor no mueve a carpetas"));
                }
                if folder.account != ctx.account || folder.path.is_empty() {
                    return Err(not_expr("the folder is in another account", "la carpeta es de otra cuenta"));
                }
                req.insert("fileinto");
                tail.push(format!("fileinto {};", quote(&folder.path)));
            }
            Action::Delete => match (&ctx.trash_path, ctx.has("fileinto")) {
                (Some(t), true) => {
                    req.insert("fileinto");
                    tail.push(format!("fileinto {};", quote(t)));
                }
                _ => tail.push("discard;".into()),
            },
            Action::Forward { to } => {
                if !ctx.has("copy") {
                    return Err(not_expr("the server can't forward a copy", "el servidor no reenvía copias"));
                }
                req.insert("copy");
                tail.push(format!("redirect :copy {};", quote(to.trim())));
            }
            Action::Redirect { to } => tail.push(format!("redirect {};", quote(to.trim()))),
            Action::AutoReply { subject, body, days } => {
                if !ctx.has("vacation") {
                    return Err(not_expr("the server can't reply automatically", "el servidor no responde solo"));
                }
                req.insert("vacation");
                tail.push(vacation_cmd(*days, subject, body, &ctx.my_addresses));
            }
            Action::Notify | Action::Silence => {
                return Err(not_expr(
                    "notifications only exist on this computer",
                    "los avisos solo existen en este equipo",
                ));
            }
            Action::Stop => stop = true,
        }
    }
    let mut out = Vec::new();
    if !flags.is_empty() {
        if !ctx.has("imap4flags") {
            return Err(not_expr("the server can't set flags or labels", "el servidor no pone marcas ni etiquetas"));
        }
        req.insert("imap4flags");
        // `addflag` va antes de `fileinto`/`keep`: así el mensaje se guarda ya marcado.
        out.push(format!("addflag {};", quote_list(&flags)));
    }
    out.extend(tail);
    if stop {
        out.push("stop;".into());
    }
    Ok(out)
}

fn vacation_cmd(days: u32, subject: &str, body: &str, mine: &[String]) -> String {
    let mut s = format!("vacation :days {}", days.clamp(1, 90));
    if !subject.trim().is_empty() {
        s.push_str(&format!(" :subject {}", quote(subject.trim())));
    }
    if !mine.is_empty() {
        s.push_str(&format!(" :addresses {}", quote_list(mine)));
    }
    s.push_str(&format!(" {};", quote(body)));
    s
}

/// Reglas (y vacaciones) → script Sieve para una cuenta.
pub fn to_sieve(rules: &[Rule], vacation: Option<&Vacation>, ctx: &GenCtx) -> Script {
    let mut req: BTreeSet<&'static str> = BTreeSet::new();
    let mut body = String::new();
    let mut out = Script::default();
    if let Some(v) = vacation.filter(|v| v.enabled && !v.body.trim().is_empty())
        && let Some(s) = gen_vacation(v, ctx, &mut req)
    {
        body.push_str(&format!("# {META}vacation\n{s}\n"));
        out.vacation = true;
    }
    for r in rules {
        if !r.enabled || r.account.is_some_and(|a| a != ctx.account) {
            continue;
        }
        if !r.prefer_server {
            out.local
                .push((r.id, Bi::new("you chose to run it on this computer", "elegiste ejecutarla en este equipo")));
            continue;
        }
        let mut local_req = BTreeSet::new();
        let tests: std::result::Result<Vec<String>, Bi> =
            r.conditions.iter().map(|c| gen_test(c, ctx, &mut local_req)).collect();
        let acts = gen_actions(r, ctx, &mut local_req);
        match (tests, acts) {
            (Ok(tests), Ok(acts)) => {
                req.extend(local_req);
                let test = match tests.as_slice() {
                    [] => "true".to_string(),
                    [one] => one.clone(),
                    many => format!("{} ({})", if r.all { "allof" } else { "anyof" }, many.join(", ")),
                };
                let meta = serde_json::json!({ "id": r.id, "name": r.name });
                body.push_str(&format!("# {META}{meta}\nif {test} {{\n"));
                for a in acts {
                    body.push_str(&format!("    {a}\n"));
                }
                body.push_str("}\n");
                out.server.push(r.id);
            }
            (Err(why), _) | (_, Err(why)) => out.local.push((r.id, why)),
        }
    }
    let mut text =
        String::from("# Generado por IureDesk. Los cambios hechos aquí se reemplazan al guardar las reglas.\n");
    if !req.is_empty() {
        let list: Vec<String> = req.iter().map(|s| s.to_string()).collect();
        text.push_str(&format!("require {};\n", if list.len() == 1 { quote(&list[0]) } else { quote_list(&list) }));
    }
    text.push_str(&body);
    out.text = text;
    out
}

fn gen_vacation(v: &Vacation, ctx: &GenCtx, req: &mut BTreeSet<&'static str>) -> Option<String> {
    if !ctx.has("vacation") {
        return None;
    }
    let cmd = vacation_cmd(v.days, &v.subject, &v.body, &ctx.my_addresses);
    let mut conds = Vec::new();
    if v.start.is_some() || v.end.is_some() {
        if !(ctx.has("date") && ctx.has("relational")) {
            return None;
        }
        req.insert("date");
        req.insert("relational");
        if let Some(s) = v.start {
            conds.push(format!("currentdate :value \"ge\" \"date\" {}", quote(&s.format("%Y-%m-%d").to_string())));
        }
        if let Some(e) = v.end {
            conds.push(format!("currentdate :value \"le\" \"date\" {}", quote(&e.format("%Y-%m-%d").to_string())));
        }
    }
    req.insert("vacation");
    Some(match conds.as_slice() {
        [] => cmd,
        [one] => format!("if {one} {{\n    {cmd}\n}}"),
        many => format!("if allof ({}) {{\n    {cmd}\n}}", many.join(", ")),
    })
}

// ------------------------------------------------------------------------------------ leer

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Tag(String),
    Str(String),
    Num(u64),
    Punct(char),
    Meta(String),
}

fn syntax(line: usize, msg: impl Into<String>) -> RulesError {
    RulesError::Syntax { line, msg: msg.into() }
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>> {
    if src.len() > MAX_SCRIPT {
        return Err(syntax(1, "script demasiado grande"));
    }
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;
    let at = |i: usize| chars.get(i).copied();
    while let Some(c) = at(i) {
        if out.len() > MAX_TOKENS {
            return Err(syntax(line, "script demasiado largo"));
        }
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            c if c.is_whitespace() => i += 1,
            '#' => {
                let start = i + 1;
                while at(i).is_some_and(|c| c != '\n') {
                    i += 1;
                }
                let text: String = chars.get(start..i).unwrap_or_default().iter().collect();
                if let Some(m) = text.trim().strip_prefix(META) {
                    out.push((Tok::Meta(m.to_string()), line));
                }
            }
            '/' if at(i + 1) == Some('*') => {
                i += 2;
                loop {
                    match at(i) {
                        None => return Err(syntax(line, "comentario sin cerrar")),
                        Some('*') if at(i + 1) == Some('/') => {
                            i += 2;
                            break;
                        }
                        Some('\n') => {
                            line += 1;
                            i += 1;
                        }
                        Some(_) => i += 1,
                    }
                }
            }
            '"' => {
                let l0 = line;
                i += 1;
                let mut s = String::new();
                loop {
                    match at(i) {
                        None => return Err(syntax(l0, "texto sin cerrar")),
                        Some('"') => {
                            i += 1;
                            break;
                        }
                        Some('\\') => {
                            if let Some(n) = at(i + 1) {
                                s.push(n);
                            }
                            i += 2;
                        }
                        Some(c) => {
                            if c == '\n' {
                                line += 1;
                            }
                            s.push(c);
                            i += 1;
                        }
                    }
                }
                out.push((Tok::Str(s.replace("\r\n", "\n")), l0));
            }
            '[' | ']' | '(' | ')' | '{' | '}' | ',' | ';' => {
                out.push((Tok::Punct(c), line));
                i += 1;
            }
            ':' => {
                i += 1;
                let start = i;
                while at(i).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    i += 1;
                }
                let tag: String = chars.get(start..i).unwrap_or_default().iter().collect();
                if tag.is_empty() {
                    return Err(syntax(line, "«:» suelto"));
                }
                out.push((Tok::Tag(tag.to_ascii_lowercase()), line));
            }
            c if c.is_ascii_digit() => {
                let start = i;
                while at(i).is_some_and(|c| c.is_ascii_digit()) {
                    i += 1;
                }
                let digits: String = chars.get(start..i).unwrap_or_default().iter().collect();
                let mut n: u64 = digits.parse().map_err(|_| syntax(line, "número fuera de rango"))?;
                let mul = match at(i).map(|c| c.to_ascii_uppercase()) {
                    Some('K') => 1u64 << 10,
                    Some('M') => 1 << 20,
                    Some('G') => 1 << 30,
                    _ => 1,
                };
                if mul > 1 {
                    i += 1;
                }
                n = n.checked_mul(mul).ok_or_else(|| syntax(line, "número fuera de rango"))?;
                out.push((Tok::Num(n), line));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while at(i).is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    i += 1;
                }
                let id: String = chars.get(start..i).unwrap_or_default().iter().collect();
                if id.eq_ignore_ascii_case("text") && at(i) == Some(':') {
                    // `text:` multilínea hasta una línea con un punto.
                    i += 1;
                    while at(i).is_some_and(|c| c != '\n') {
                        i += 1;
                    }
                    i += 1;
                    line += 1;
                    let l0 = line;
                    let mut s = String::new();
                    loop {
                        let start = i;
                        while at(i).is_some_and(|c| c != '\n') {
                            i += 1;
                        }
                        if at(i).is_none() && start == i {
                            return Err(syntax(l0, "texto multilínea sin cerrar"));
                        }
                        let l: String = chars.get(start..i).unwrap_or_default().iter().collect();
                        let l = l.trim_end_matches('\r');
                        i += 1;
                        line += 1;
                        if l == "." {
                            break;
                        }
                        s.push_str(l.strip_prefix('.').filter(|_| l.starts_with("..")).unwrap_or(l));
                        s.push('\n');
                        if at(i).is_none() {
                            return Err(syntax(l0, "texto multilínea sin cerrar"));
                        }
                    }
                    out.push((Tok::Str(s), l0));
                } else {
                    out.push((Tok::Ident(id.to_ascii_lowercase()), line));
                }
            }
            other => return Err(syntax(line, format!("carácter inesperado «{other}»"))),
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Arg {
    Tag(String),
    Num(u64),
    Strs(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Test {
    name: String,
    args: Vec<Arg>,
    tests: Vec<Test>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Command {
    name: String,
    args: Vec<Arg>,
    tests: Vec<Test>,
    block: Option<Vec<Command>>,
    meta: Option<String>,
    line: usize,
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }
    fn line(&self) -> usize {
        self.toks.get(self.pos).or(self.toks.last()).map_or(1, |(_, l)| *l)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|(t, _)| t.clone());
        self.pos += 1;
        t
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Punct(c)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn commands(&mut self, depth: usize) -> Result<Vec<Command>> {
        if depth > MAX_DEPTH {
            return Err(syntax(self.line(), "demasiado anidado"));
        }
        let mut out = Vec::new();
        let mut meta = None;
        loop {
            match self.peek() {
                None => break,
                Some(Tok::Punct('}')) if depth > 0 => break,
                Some(Tok::Meta(m)) => {
                    meta = Some(m.clone());
                    self.pos += 1;
                }
                Some(Tok::Ident(_)) => {
                    let mut c = self.command(depth)?;
                    c.meta = meta.take();
                    out.push(c);
                }
                Some(_) => return Err(syntax(self.line(), "se esperaba un comando")),
            }
        }
        Ok(out)
    }

    fn command(&mut self, depth: usize) -> Result<Command> {
        let line = self.line();
        let Some(Tok::Ident(name)) = self.next() else { return Err(syntax(line, "se esperaba un comando")) };
        let (args, tests) = self.arguments(depth)?;
        let block = if self.eat(';') {
            None
        } else if self.eat('{') {
            let b = self.commands(depth + 1)?;
            if !self.eat('}') {
                return Err(syntax(self.line(), "falta «}»"));
            }
            Some(b)
        } else {
            return Err(syntax(self.line(), "falta «;»"));
        };
        Ok(Command { name, args, tests, block, meta: None, line })
    }

    fn arguments(&mut self, depth: usize) -> Result<(Vec<Arg>, Vec<Test>)> {
        if depth > MAX_DEPTH {
            return Err(syntax(self.line(), "demasiado anidado"));
        }
        let mut args = Vec::new();
        loop {
            match self.peek().cloned() {
                Some(Tok::Tag(t)) => {
                    self.pos += 1;
                    args.push(Arg::Tag(t));
                }
                Some(Tok::Num(n)) => {
                    self.pos += 1;
                    args.push(Arg::Num(n));
                }
                Some(Tok::Str(s)) => {
                    self.pos += 1;
                    args.push(Arg::Strs(vec![s]));
                }
                Some(Tok::Punct('[')) => {
                    self.pos += 1;
                    let mut list = Vec::new();
                    loop {
                        match self.next() {
                            Some(Tok::Str(s)) => list.push(s),
                            _ => return Err(syntax(self.line(), "lista mal formada")),
                        }
                        if self.eat(']') {
                            break;
                        }
                        if !self.eat(',') {
                            return Err(syntax(self.line(), "falta «,» o «]»"));
                        }
                    }
                    args.push(Arg::Strs(list));
                }
                _ => break,
            }
        }
        let mut tests = Vec::new();
        if let Some(Tok::Ident(_)) = self.peek() {
            tests.push(self.test(depth + 1)?);
        } else if self.eat('(') {
            loop {
                tests.push(self.test(depth + 1)?);
                if self.eat(')') {
                    break;
                }
                if !self.eat(',') {
                    return Err(syntax(self.line(), "falta «,» o «)»"));
                }
            }
        }
        Ok((args, tests))
    }

    fn test(&mut self, depth: usize) -> Result<Test> {
        let line = self.line();
        let Some(Tok::Ident(name)) = self.next() else { return Err(syntax(line, "se esperaba una prueba")) };
        let (args, tests) = self.arguments(depth)?;
        Ok(Test { name, args, tests })
    }
}

/// Lo que se entendió de un script.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    /// Reglas.
    pub rules: Vec<Rule>,
    /// Respuesta de vacaciones.
    pub vacation: Option<Vacation>,
    /// Partes que no se pudieron traducir.
    pub skipped: usize,
}

/// Contexto para leer.
pub struct ReadCtx<'a> {
    /// Cuenta del script.
    pub account: AccountId,
    /// Ruta de la papelera (`fileinto` a ella es «Enviarlo a la papelera»).
    pub trash_path: Option<&'a str>,
    /// Carpeta local de una ruta.
    pub resolve: &'a dyn Fn(&str) -> Option<FolderRef>,
}

fn strs(args: &[Arg]) -> Vec<&Vec<String>> {
    args.iter().filter_map(|a| if let Arg::Strs(s) = a { Some(s) } else { None }).collect()
}

fn has_tag(args: &[Arg], t: &str) -> bool {
    args.iter().any(|a| matches!(a, Arg::Tag(x) if x == t))
}

fn tag_value<'a>(args: &'a [Arg], t: &str) -> Option<&'a Arg> {
    let pos = args.iter().position(|a| matches!(a, Arg::Tag(x) if x == t))?;
    args.get(pos + 1)
}

fn field_of(names: &[String]) -> Option<Field> {
    let lower: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    Some(match lower.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["from"] => Field::From,
        ["to"] => Field::To,
        ["cc"] => Field::Cc,
        ["to", "cc"] | ["cc", "to"] => Field::Recipients,
        ["subject"] => Field::Subject,
        ["list-id"] => Field::ListId,
        [h] => Field::Header((*h).to_string()),
        _ => return None,
    })
}

fn unescape_wild(s: &str) -> Option<String> {
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '*' || c == '?' {
            return None;
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// Patrón de `:matches` → operación nuestra.
fn from_matches(p: &str) -> Option<(TextOp, String)> {
    let starts = p.ends_with('*') && !p.ends_with("\\*");
    let ends = p.starts_with('*');
    match (ends, starts) {
        (true, true) if p.len() >= 2 => Some((TextOp::Contains, unescape_wild(p.get(1..p.len() - 1)?)?)),
        (false, true) => Some((TextOp::StartsWith, unescape_wild(p.get(..p.len() - 1)?)?)),
        (true, false) => Some((TextOp::EndsWith, unescape_wild(p.get(1..)?)?)),
        _ => Some((TextOp::Is, unescape_wild(p)?)),
    }
}

fn read_test(t: &Test) -> Option<Vec<Condition>> {
    let s = strs(&t.args);
    let match_op = |args: &[Arg], v: &str| -> Option<(TextOp, String)> {
        if has_tag(args, "contains") {
            Some((TextOp::Contains, v.to_string()))
        } else if has_tag(args, "regex") {
            Some((TextOp::Regex, v.to_string()))
        } else if has_tag(args, "matches") {
            from_matches(v)
        } else {
            Some((TextOp::Is, v.to_string()))
        }
    };
    Some(match t.name.as_str() {
        "true" => Vec::new(),
        "false" => vec![Condition::Account { account: AccountId(u64::MAX) }],
        "header" => {
            let [names, values] = s.as_slice() else { return None };
            let field = field_of(names)?;
            let [v] = values.as_slice() else { return None };
            let (op, value) = match_op(&t.args, v)?;
            vec![Condition::Text { field, op, value }]
        }
        "address" => {
            let [names, values] = s.as_slice() else { return None };
            let field = field_of(names).filter(Field::is_address)?;
            if has_tag(&t.args, "domain") {
                let d = values.first()?;
                let plain = unescape_wild(d).or_else(|| d.strip_prefix("*.").and_then(unescape_wild))?;
                return Some(vec![Condition::Domain { field, domain: plain }]);
            }
            let [v] = values.as_slice() else { return None };
            let (op, value) = match_op(&t.args, v)?;
            vec![Condition::Text { field, op, value }]
        }
        "body" => {
            let [values] = s.as_slice() else { return None };
            let [v] = values.as_slice() else { return None };
            let (op, value) = match_op(&t.args, v)?;
            vec![Condition::Text { field: Field::Body, op, value }]
        }
        "size" => {
            let n = t.args.iter().find_map(|a| if let Arg::Num(n) = a { Some(*n) } else { None })?;
            if has_tag(&t.args, "over") {
                vec![Condition::SizeOver { bytes: n }]
            } else {
                vec![Condition::SizeUnder { bytes: n }]
            }
        }
        "exists" => {
            let [names] = s.as_slice() else { return None };
            match names.iter().map(|n| n.to_ascii_lowercase()).collect::<Vec<_>>().as_slice() {
                [n] if n == "in-reply-to" => vec![Condition::IsReply],
                [n] if n == "list-id" => vec![Condition::IsList],
                _ => return None,
            }
        }
        "not" => {
            let [inner] = t.tests.as_slice() else { return None };
            let mut c = read_test(inner)?;
            match c.as_mut_slice() {
                [Condition::Text { op: op @ TextOp::Contains, .. }] => *op = TextOp::NotContains,
                _ => return None,
            }
            c
        }
        _ => return None,
    })
}

fn read_actions(cmds: &[Command], ctx: &ReadCtx<'_>) -> Option<Vec<Action>> {
    let mut out = Vec::new();
    for c in cmds {
        let s = strs(&c.args);
        match c.name.as_str() {
            "fileinto" => {
                let p = s.first()?.first()?;
                if ctx.trash_path.is_some_and(|t| t == p) {
                    out.push(Action::Delete);
                } else {
                    let folder = (ctx.resolve)(p).unwrap_or_else(|| FolderRef {
                        id: iuredesk_model::FolderId(0),
                        account: ctx.account,
                        path: p.clone(),
                        name: p.rsplit(['/', '.']).next().unwrap_or(p).to_string(),
                    });
                    out.push(Action::MoveTo { folder });
                }
            }
            "addflag" | "setflag" => {
                for f in
                    s.into_iter().flatten().flat_map(|f| f.split_whitespace().map(str::to_string).collect::<Vec<_>>())
                {
                    out.push(match f.to_ascii_lowercase().as_str() {
                        "\\seen" => Action::MarkRead,
                        "\\flagged" => Action::Flag,
                        _ => Action::Label { name: f },
                    });
                }
            }
            "discard" => out.push(Action::Delete),
            "redirect" => {
                let to = s.first()?.first()?.clone();
                out.push(if has_tag(&c.args, "copy") { Action::Forward { to } } else { Action::Redirect { to } });
            }
            "vacation" => out.push(read_vacation(c)?),
            "keep" => {}
            "stop" => out.push(Action::Stop),
            _ => return None,
        }
    }
    Some(out)
}

fn read_vacation(c: &Command) -> Option<Action> {
    let days = match tag_value(&c.args, "days") {
        Some(Arg::Num(n)) => u32::try_from(*n).unwrap_or(7).clamp(1, 90),
        _ => 7,
    };
    let subject = match tag_value(&c.args, "subject") {
        Some(Arg::Strs(v)) => v.first().cloned().unwrap_or_default(),
        _ => String::new(),
    };
    // El texto es el último argumento de texto que no es valor de una etiqueta.
    let mut reason = None;
    let mut skip_next = false;
    for a in &c.args {
        match a {
            Arg::Tag(t) => {
                skip_next = matches!(t.as_str(), "days" | "subject" | "from" | "addresses" | "handle" | "seconds")
            }
            Arg::Strs(v) if !skip_next => reason = v.first().cloned(),
            _ => skip_next = false,
        }
        if !matches!(a, Arg::Tag(_)) {
            skip_next = false;
        }
    }
    Some(Action::AutoReply { subject, body: reason?, days })
}

fn read_vacation_dates(t: &Test) -> Option<(Option<NaiveDate>, Option<NaiveDate>)> {
    let tests: Vec<&Test> = if t.name == "allof" { t.tests.iter().collect() } else { vec![t] };
    let (mut start, mut end) = (None, None);
    for x in tests {
        if x.name != "currentdate" {
            return None;
        }
        let s = strs(&x.args);
        let rel = s.first()?.first()?.as_str();
        let date = NaiveDate::parse_from_str(s.get(2)?.first()?, "%Y-%m-%d").ok()?;
        match rel {
            "ge" | "gt" => start = Some(date),
            "le" | "lt" => end = Some(date),
            _ => return None,
        }
    }
    Some((start, end))
}

fn to_vacation(a: Action, start: Option<NaiveDate>, end: Option<NaiveDate>) -> Option<Vacation> {
    let Action::AutoReply { subject, body, days } = a else { return None };
    Some(Vacation { enabled: true, start, end, subject, body, days, contacts_only: false })
}

/// Script → reglas que entendemos.
pub fn from_sieve(src: &str, ctx: &ReadCtx<'_>) -> Result<Imported> {
    let mut p = Parser { toks: lex(src)?, pos: 0 };
    let cmds = p.commands(0)?;
    let mut out = Imported::default();
    let mut next_local = 1u64;
    for c in cmds {
        match c.name.as_str() {
            "require" => {}
            "vacation" => match read_vacation(&c).and_then(|a| to_vacation(a, None, None)) {
                Some(v) => out.vacation = Some(v),
                None => out.skipped += 1,
            },
            "if" => {
                let Some(block) = c.block.as_ref() else {
                    out.skipped += 1;
                    continue;
                };
                let [test] = c.tests.as_slice() else {
                    out.skipped += 1;
                    continue;
                };
                // Vacaciones con fechas.
                if let [only] = block.as_slice()
                    && only.name == "vacation"
                    && let Some((s, e)) = read_vacation_dates(test)
                {
                    match read_vacation(only).and_then(|a| to_vacation(a, s, e)) {
                        Some(v) => out.vacation = Some(v),
                        None => out.skipped += 1,
                    }
                    continue;
                }
                let (all, tests): (bool, Vec<&Test>) = match test.name.as_str() {
                    "allof" => (true, test.tests.iter().collect()),
                    "anyof" => (false, test.tests.iter().collect()),
                    _ => (true, vec![test]),
                };
                let conds: Option<Vec<Vec<Condition>>> = tests.iter().map(|t| read_test(t)).collect();
                let actions = read_actions(block, ctx);
                let (Some(conds), Some(actions)) = (conds, actions) else {
                    out.skipped += 1;
                    continue;
                };
                let meta: Option<serde_json::Value> = c.meta.as_deref().and_then(|m| serde_json::from_str(m).ok());
                let id =
                    meta.as_ref().and_then(|m| m.get("id")).and_then(serde_json::Value::as_u64).unwrap_or_else(|| {
                        next_local += 1;
                        0
                    });
                let name = meta
                    .as_ref()
                    .and_then(|m| m.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Sieve {}", c.line));
                let conditions: Vec<Condition> = conds.into_iter().flatten().collect();
                let mut rule = Rule { id, name, all, conditions, actions, account: None, ..Rule::default() };
                // `true` como única condición de una cuenta concreta.
                if rule.conditions.iter().any(|c| matches!(c, Condition::Account { account } if account.0 == u64::MAX))
                {
                    rule.enabled = false;
                    rule.conditions.retain(|c| !matches!(c, Condition::Account { .. }));
                }
                out.rules.push(rule);
            }
            _ => out.skipped += 1,
        }
    }
    Ok(out)
}

/// Comprueba la sintaxis de un script (sin interpretarlo).
pub fn check(src: &str) -> Result<()> {
    let mut p = Parser { toks: lex(src)?, pos: 0 };
    p.commands(0).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuredesk_model::FolderId;

    fn ctx_all() -> GenCtx {
        GenCtx {
            account: AccountId(1),
            extensions: ["fileinto", "imap4flags", "copy", "vacation", "regex", "body", "date", "relational"]
                .into_iter()
                .map(String::from)
                .collect(),
            trash_path: Some("Trash".into()),
            my_addresses: vec!["yo@despacho.example".into()],
        }
    }

    fn folder(path: &str) -> FolderRef {
        FolderRef {
            id: FolderId(7),
            account: AccountId(1),
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
        }
    }

    fn sample_rules() -> Vec<Rule> {
        let t = |field, op, v: &str| Condition::Text { field, op, value: v.into() };
        vec![
            Rule {
                id: 1,
                name: "Clientes \"VIP\"".into(),
                conditions: vec![
                    Condition::Domain { field: Field::From, domain: "cliente.example".into() },
                    t(Field::Subject, TextOp::Contains, "factura"),
                ],
                actions: vec![
                    Action::Label { name: "Facturas".into() },
                    Action::Flag,
                    Action::MoveTo { folder: folder("INBOX/Clientes") },
                    Action::Stop,
                ],
                ..Rule::default()
            },
            Rule {
                id: 2,
                name: "Boletines".into(),
                all: false,
                conditions: vec![Condition::IsList, t(Field::From, TextOp::StartsWith, "news*")],
                actions: vec![Action::MarkRead, Action::Delete],
                ..Rule::default()
            },
            Rule {
                id: 3,
                name: "Reenviar".into(),
                conditions: vec![
                    t(Field::Recipients, TextOp::EndsWith, "@socios.example"),
                    t(Field::Body, TextOp::NotContains, "confidencial"),
                    Condition::SizeOver { bytes: 1024 * 1024 },
                    t(Field::Subject, TextOp::Regex, r"^\[caso \d+\]"),
                    t(Field::Header("x-prioridad".into()), TextOp::Is, "alta"),
                    Condition::IsReply,
                ],
                actions: vec![Action::Forward { to: "socio@despacho.example".into() }],
                ..Rule::default()
            },
            Rule {
                id: 4,
                name: "Redirigir".into(),
                conditions: vec![Condition::SizeUnder { bytes: 500 }],
                actions: vec![
                    Action::Redirect { to: "otro@despacho.example".into() },
                    Action::AutoReply { subject: "Recibido".into(), body: "Gracias,\nlo reviso.".into(), days: 3 },
                ],
                ..Rule::default()
            },
        ]
    }

    #[test]
    fn round_trip() {
        let rules = sample_rules();
        let vac = Vacation {
            enabled: true,
            start: NaiveDate::from_ymd_opt(2026, 12, 20),
            end: NaiveDate::from_ymd_opt(2027, 1, 2),
            subject: "Fuera de la oficina".into(),
            body: "Vuelvo el 2 de enero.".into(),
            days: 5,
            contacts_only: false,
        };
        let ctx = ctx_all();
        let s = to_sieve(&rules, Some(&vac), &ctx);
        assert_eq!(s.server, vec![1, 2, 3, 4], "{s:#?}");
        assert!(s.local.is_empty());
        assert!(s.vacation);
        assert!(s.text.contains("require ["), "{}", s.text);
        check(&s.text).unwrap();
        let resolve = |p: &str| (p == "INBOX/Clientes").then(|| folder(p));
        let rctx = ReadCtx { account: AccountId(1), trash_path: Some("Trash"), resolve: &resolve };
        let back = from_sieve(&s.text, &rctx).unwrap();
        assert_eq!(back.skipped, 0);
        assert_eq!(back.vacation.as_ref(), Some(&vac));
        assert_eq!(back.rules.len(), rules.len());
        for (a, b) in rules.iter().zip(&back.rules) {
            assert_eq!(a, b, "\n{}", s.text);
        }
    }

    #[test]
    fn local_only_when_server_cannot() {
        let rules = vec![
            Rule { id: 1, conditions: vec![Condition::HasAttachment], actions: vec![Action::Flag], ..Rule::default() },
            Rule { id: 2, actions: vec![Action::Notify], ..Rule::default() },
            Rule { id: 3, actions: vec![Action::MoveTo { folder: folder("X") }], ..Rule::default() },
            Rule { id: 4, prefer_server: false, actions: vec![Action::Flag], ..Rule::default() },
            Rule { id: 5, account: Some(AccountId(2)), actions: vec![Action::Flag], ..Rule::default() },
            Rule { id: 6, actions: vec![Action::Delete], ..Rule::default() },
        ];
        // Servidor mínimo: sin extensiones.
        let ctx = GenCtx { account: AccountId(1), ..Default::default() };
        let s = to_sieve(&rules, None, &ctx);
        assert_eq!(s.server, vec![6], "{s:#?}");
        assert!(s.text.contains("discard;"));
        assert_eq!(s.local.iter().map(|(id, _)| *id).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        assert!(s.local[0].1.es.contains("adjuntos"));
        let vac = Vacation { enabled: true, body: "x".into(), ..Default::default() };
        assert!(!to_sieve(&[], Some(&vac), &ctx).vacation, "sin `vacation`: local");
    }

    #[test]
    fn reads_hand_written_scripts() {
        let src = r#"require ["fileinto", "imap4flags"];
/* comentario
   de varias líneas */
if address :is "from" "jefe@empresa.example" { addflag "\\Flagged"; }
if header :contains "subject" ["oferta"] { fileinto "Trash"; }
if exists "list-id" { fileinto "Junk"; } elsif true { keep; }
vacation :days 10 :subject "Ausente" text:
Estoy fuera.
..punto
.
;
if anyof (not header :contains "from" "a", size :over 10K) { discard; stop; }
"#;
        let resolve = |_: &str| None;
        let ctx = ReadCtx { account: AccountId(1), trash_path: Some("Trash"), resolve: &resolve };
        let r = from_sieve(src, &ctx).unwrap();
        assert_eq!(r.rules.len(), 4, "{r:#?}");
        assert_eq!(r.rules[0].actions, vec![Action::Flag]);
        assert_eq!(r.rules[1].actions, vec![Action::Delete]);
        assert!(matches!(&r.rules[2].actions[0], Action::MoveTo { folder } if folder.path == "Junk"));
        assert_eq!(r.skipped, 1, "elsif");
        let v = r.vacation.unwrap();
        assert_eq!((v.days, v.subject.as_str(), v.body.as_str()), (10, "Ausente", "Estoy fuera.\n.punto\n"));
        assert!(!r.rules[3].all);
        assert_eq!(r.rules[3].conditions[1], Condition::SizeOver { bytes: 10 * 1024 });
        assert!(matches!(&r.rules[3].conditions[0], Condition::Text { op: TextOp::NotContains, .. }));
    }

    #[test]
    fn syntax_errors_have_lines() {
        assert!(matches!(check("if true {\n keep;\n"), Err(RulesError::Syntax { .. })));
        assert!(matches!(check("keep"), Err(RulesError::Syntax { .. })));
        assert!(matches!(check("fileinto \"x"), Err(RulesError::Syntax { line: 1, .. })));
        assert!(check(&"if true {".repeat(100)).is_err());
        assert!(check("").is_ok());
    }

    proptest::proptest! {
        #[test]
        fn parser_never_panics(s in "\\PC{0,400}") {
            let _ = check(&s);
        }

        #[test]
        fn quoted_values_round_trip(v in "[^\\x00-\\x1f]{1,40}") {
            let r = Rule {
                id: 9,
                name: "x".into(),
                conditions: vec![Condition::Text { field: Field::Subject, op: TextOp::Contains, value: v.trim().to_string() }],
                actions: vec![Action::Flag],
                ..Rule::default()
            };
            proptest::prop_assume!(!v.trim().is_empty());
            let s = to_sieve(std::slice::from_ref(&r), None, &ctx_all());
            let resolve = |_: &str| None;
            let back = from_sieve(&s.text, &ReadCtx { account: AccountId(1), trash_path: None, resolve: &resolve }).unwrap();
            proptest::prop_assert_eq!(&back.rules[0].conditions, &r.conditions);
        }
    }
}
