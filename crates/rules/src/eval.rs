//! Evaluación: qué reglas se cumplen para un mensaje y qué acciones tocan, en orden.
//!
//! Las expresiones regulares se compilan una vez ([`CompiledRules`]) con `regex` (tiempo lineal,
//! sin retroceso) y topes de tamaño: una regla hostil o un correo enorme no cuelgan la app.

use std::collections::HashSet;

use regex::{Regex, RegexBuilder};

use crate::facts::{Facts, mailbox_text};
use crate::rule::{Action, Condition, Field, Rule, TextOp};

/// Longitud máxima de un patrón.
const MAX_PATTERN: usize = 500;
/// Tamaño máximo del autómata compilado.
const REGEX_SIZE: usize = 1 << 20;

/// Compila un patrón acotado, sin distinguir mayúsculas.
pub fn compile_regex(p: &str) -> Result<Regex, String> {
    if p.len() > MAX_PATTERN {
        return Err("demasiado largo".into());
    }
    RegexBuilder::new(p)
        .case_insensitive(true)
        .size_limit(REGEX_SIZE)
        .dfa_size_limit(REGEX_SIZE)
        .nest_limit(50)
        .build()
        .map_err(|e| e.to_string().lines().last().unwrap_or("").trim().to_string())
}

/// Reglas listas para evaluar (patrones compilados).
#[derive(Clone, Debug, Default)]
pub struct CompiledRules {
    rules: Vec<(Rule, Vec<Option<Regex>>)>,
}

/// Lo que hay que hacer con un mensaje.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Reglas cumplidas (ids), en orden.
    pub matched: Vec<u64>,
    /// Acciones, sin repetir y sin `Stop`.
    pub actions: Vec<(u64, Action)>,
}

impl Plan {
    /// No hay nada que hacer.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

impl CompiledRules {
    /// Compila las reglas activas (las de patrón roto se omiten).
    pub fn new(rules: &[Rule]) -> Self {
        let rules = rules
            .iter()
            .filter(|r| r.enabled)
            .map(|r| {
                let rx = r
                    .conditions
                    .iter()
                    .map(|c| match c {
                        Condition::Text { op: TextOp::Regex, value, .. } => compile_regex(value).ok(),
                        _ => None,
                    })
                    .collect();
                (r.clone(), rx)
            })
            .collect();
        Self { rules }
    }

    /// Número de reglas activas.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Sin reglas activas.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Plan para un mensaje. `skip` son reglas que ya corren en el servidor para esa cuenta.
    pub fn plan(&self, f: &Facts, skip: &HashSet<u64>) -> Plan {
        let mut plan = Plan::default();
        let mut placed = false;
        for (rule, rx) in &self.rules {
            if skip.contains(&rule.id) {
                continue;
            }
            if rule.account.is_some_and(|a| a != f.env.account) {
                continue;
            }
            if !rule_matches(rule, rx, f) {
                continue;
            }
            plan.matched.push(rule.id);
            let mut stop = false;
            for a in &rule.actions {
                match a {
                    Action::Stop => stop = true,
                    // Un mensaje solo puede acabar en un sitio: gana la primera regla.
                    Action::MoveTo { .. } | Action::Delete => {
                        if !placed {
                            placed = true;
                            plan.actions.push((rule.id, a.clone()));
                        }
                    }
                    other => {
                        if !plan.actions.iter().any(|(_, x)| x == other) {
                            plan.actions.push((rule.id, other.clone()));
                        }
                    }
                }
            }
            if stop {
                break;
            }
        }
        plan
    }
}

/// ¿Se cumple la regla?
pub fn matches(rule: &Rule, f: &Facts) -> bool {
    let rx: Vec<Option<Regex>> = rule
        .conditions
        .iter()
        .map(|c| match c {
            Condition::Text { op: TextOp::Regex, value, .. } => compile_regex(value).ok(),
            _ => None,
        })
        .collect();
    rule_matches(rule, &rx, f)
}

fn rule_matches(rule: &Rule, rx: &[Option<Regex>], f: &Facts) -> bool {
    if rule.conditions.is_empty() {
        return true;
    }
    let mut results =
        rule.conditions.iter().enumerate().map(|(i, c)| cond_matches(c, rx.get(i).and_then(Option::as_ref), f));
    if rule.all { results.all(|b| b) } else { results.any(|b| b) }
}

fn field_values(field: &Field, f: &Facts) -> Vec<String> {
    let env = &f.env;
    match field {
        Field::From => env.from.iter().map(mailbox_text).collect(),
        Field::To => env.to.iter().map(mailbox_text).collect(),
        Field::Cc => env.cc.iter().map(mailbox_text).collect(),
        Field::Recipients => env.to.iter().chain(env.cc.iter()).map(mailbox_text).collect(),
        Field::Subject => vec![env.subject.clone()],
        Field::Body => vec![f.body.clone()],
        Field::ListId => f.headers.get("list-id").map(str::to_string).into_iter().collect(),
        Field::Header(h) => f.headers.all(h).map(str::to_string).collect(),
    }
}

fn addresses(field: &Field, f: &Facts) -> Vec<String> {
    let env = &f.env;
    let list: Vec<&iuredesk_model::Mailbox> = match field {
        Field::From => env.from.iter().collect(),
        Field::To => env.to.iter().collect(),
        Field::Cc => env.cc.iter().collect(),
        Field::Recipients => env.to.iter().chain(env.cc.iter()).collect(),
        _ => Vec::new(),
    };
    list.into_iter().map(|m| crate::norm_addr(&m.addr)).collect()
}

fn text_matches(op: TextOp, value: &str, rx: Option<&Regex>, hay: &[String]) -> bool {
    let needle = value.trim().to_lowercase();
    let one = |h: &String| -> bool {
        let h = h.to_lowercase();
        match op {
            TextOp::Contains | TextOp::NotContains => h.contains(&needle),
            TextOp::Is => h.trim() == needle,
            TextOp::StartsWith => h.trim_start().starts_with(&needle),
            TextOp::EndsWith => h.trim_end().ends_with(&needle),
            TextOp::Regex => rx.is_some_and(|r| r.is_match(&h)),
        }
    };
    let any = hay.iter().any(one);
    if op == TextOp::NotContains { !any } else { any }
}

fn cond_matches(c: &Condition, rx: Option<&Regex>, f: &Facts) -> bool {
    match c {
        Condition::Text { field, op, value } => {
            let mut hay = field_values(field, f);
            // «Es» con una dirección compara también la dirección sola.
            if field.is_address() && matches!(op, TextOp::Is | TextOp::StartsWith | TextOp::EndsWith) {
                hay.extend(addresses(field, f));
            }
            text_matches(*op, value, rx, &hay)
        }
        Condition::Domain { field, domain } => {
            let want = domain.trim().trim_start_matches('@').to_lowercase();
            !want.is_empty()
                && addresses(field, f).iter().any(|a| {
                    let d = crate::domain_of(a);
                    d == want || d.ends_with(&format!(".{want}"))
                })
        }
        Condition::HasAttachment => f.env.has_attachments,
        Condition::SizeOver { bytes } => f.env.size > *bytes,
        Condition::SizeUnder { bytes } => f.env.size < *bytes,
        Condition::Account { account } => f.env.account == *account,
        Condition::InFolder { folder } => f.env.folder == *folder,
        Condition::IsReply => f.is_reply(),
        Condition::IsList => f.is_list(),
        Condition::KnownContact => f.known_sender,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Headers;
    use crate::rule::FolderRef;
    use iuredesk_model::{AccountId, FolderId, Mailbox, MessageEnvelope};

    fn facts() -> Facts {
        Facts {
            env: MessageEnvelope {
                account: AccountId(1),
                folder: FolderId(1),
                from: vec![Mailbox::new("Ana Pérez", "Ana@Cliente.Example.com")],
                to: vec![Mailbox::addr("yo@despacho.example")],
                cc: vec![Mailbox::addr("pablo@despacho.example")],
                subject: "Re: Factura 2026-0042".into(),
                size: 2 * 1024 * 1024,
                has_attachments: true,
                in_reply_to: Some("x@y".into()),
                ..Default::default()
            },
            headers: Headers::from_pairs([("List-Id", "Clientes <clientes.example.com>")]),
            body: "Adjunto la factura del mes.".into(),
            known_sender: true,
        }
    }

    fn rule(conds: Vec<Condition>, all: bool) -> Rule {
        Rule { id: 1, conditions: conds, all, actions: vec![Action::Flag], ..Rule::default() }
    }

    fn text(field: Field, op: TextOp, v: &str) -> Condition {
        Condition::Text { field, op, value: v.into() }
    }

    /// Matriz: cada condición con un caso que se cumple y uno que no.
    #[test]
    fn condition_matrix() {
        let f = facts();
        let cases: Vec<(Condition, bool)> = vec![
            (text(Field::From, TextOp::Contains, "ana@cliente"), true),
            (text(Field::From, TextOp::Contains, "pérez"), true),
            (text(Field::From, TextOp::Contains, "luis"), false),
            (text(Field::From, TextOp::NotContains, "luis"), true),
            (text(Field::From, TextOp::Is, "ana@cliente.example.com"), true),
            (text(Field::From, TextOp::EndsWith, "example.com"), true),
            (text(Field::To, TextOp::Contains, "yo@"), true),
            (text(Field::Cc, TextOp::Contains, "yo@"), false),
            (text(Field::Recipients, TextOp::Contains, "pablo"), true),
            (text(Field::Subject, TextOp::StartsWith, "re: factura"), true),
            (text(Field::Subject, TextOp::Regex, r"factura \d{4}-\d+"), true),
            (text(Field::Subject, TextOp::Regex, r"^presupuesto"), false),
            (text(Field::Body, TextOp::Contains, "FACTURA DEL MES"), true),
            (text(Field::ListId, TextOp::Contains, "clientes.example.com"), true),
            (text(Field::Header("x-nada".into()), TextOp::Contains, "a"), false),
            (Condition::Domain { field: Field::From, domain: "example.com".into() }, true),
            (Condition::Domain { field: Field::From, domain: "@cliente.example.com".into() }, true),
            (Condition::Domain { field: Field::From, domain: "ample.com".into() }, false),
            (Condition::Domain { field: Field::To, domain: "despacho.example".into() }, true),
            (Condition::HasAttachment, true),
            (Condition::SizeOver { bytes: 1024 * 1024 }, true),
            (Condition::SizeUnder { bytes: 1024 * 1024 }, false),
            (Condition::Account { account: AccountId(1) }, true),
            (Condition::Account { account: AccountId(2) }, false),
            (Condition::InFolder { folder: FolderId(1) }, true),
            (Condition::IsReply, true),
            (Condition::IsList, true),
            (Condition::KnownContact, true),
        ];
        for (c, want) in cases {
            assert_eq!(matches(&rule(vec![c.clone()], true), &f), want, "{c:?}");
        }
    }

    #[test]
    fn all_any_stop_and_first_move_wins() {
        let f = facts();
        let yes = text(Field::From, TextOp::Contains, "ana");
        let no = text(Field::From, TextOp::Contains, "zzz");
        assert!(!matches(&rule(vec![yes.clone(), no.clone()], true), &f));
        assert!(matches(&rule(vec![yes.clone(), no.clone()], false), &f));
        assert!(matches(&rule(vec![], true), &f), "sin condiciones: todo");

        let folder = |id: u64| FolderRef { id: FolderId(id), ..Default::default() };
        let rules = vec![
            Rule {
                id: 1,
                conditions: vec![yes.clone()],
                actions: vec![Action::MoveTo { folder: folder(5) }, Action::Flag],
                ..Rule::default()
            },
            Rule {
                id: 2,
                conditions: vec![yes.clone()],
                actions: vec![Action::MoveTo { folder: folder(6) }, Action::Flag, Action::Stop],
                ..Rule::default()
            },
            Rule { id: 3, conditions: vec![yes.clone()], actions: vec![Action::MarkRead], ..Rule::default() },
            Rule { id: 4, enabled: false, conditions: vec![], actions: vec![Action::Delete], ..Rule::default() },
        ];
        let c = CompiledRules::new(&rules);
        assert_eq!(c.len(), 3);
        let p = c.plan(&f, &HashSet::new());
        assert_eq!(p.matched, vec![1, 2]);
        assert_eq!(p.actions, vec![(1, Action::MoveTo { folder: folder(5) }), (1, Action::Flag)]);
        // La regla 1 corre en el servidor: aquí se omite.
        let p = c.plan(&f, &HashSet::from([1]));
        assert_eq!(p.actions[0], (2, Action::MoveTo { folder: folder(6) }));
        // Regla de otra cuenta.
        let other = vec![Rule { id: 9, account: Some(AccountId(7)), actions: vec![Action::Flag], ..Rule::default() }];
        assert!(CompiledRules::new(&other).plan(&f, &HashSet::new()).is_empty());
    }

    #[test]
    fn hostile_patterns_are_bounded() {
        assert!(compile_regex(&"a".repeat(600)).is_err());
        assert!(compile_regex("(a{1000}){1000}").is_err(), "autómata enorme");
        let r = compile_regex("(a+)+$").unwrap();
        // Tiempo lineal: no hay retroceso catastrófico.
        let hay = format!("{}b", "a".repeat(100_000));
        let t = std::time::Instant::now();
        assert!(!r.is_match(&hay));
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
    }
}
