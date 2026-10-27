//! Instrucciones de cada tarea en `prompts/<tarea>.<es|en>.md`, incluidas en el binario, con
//! marcadores `{{nombre}}` que se sustituyen en una sola pasada (un valor nunca se re-expande).

use crate::input::Lang;

/// Tarea con instrucciones propias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptId {
    DraftReply,
    Rewrite,
    SummarizeThread,
    Extract,
    Classify,
    MeetingMinutes,
    TriageToday,
    DocumentOutline,
}

impl PromptId {
    pub const ALL: [PromptId; 8] = [
        PromptId::DraftReply,
        PromptId::Rewrite,
        PromptId::SummarizeThread,
        PromptId::Extract,
        PromptId::Classify,
        PromptId::MeetingMinutes,
        PromptId::TriageToday,
        PromptId::DocumentOutline,
    ];

    /// Nombre del archivo (sin idioma ni extensión).
    pub fn name(self) -> &'static str {
        match self {
            PromptId::DraftReply => "draft_reply",
            PromptId::Rewrite => "rewrite",
            PromptId::SummarizeThread => "summarize_thread",
            PromptId::Extract => "extract",
            PromptId::Classify => "classify",
            PromptId::MeetingMinutes => "meeting_minutes",
            PromptId::TriageToday => "triage_today",
            PromptId::DocumentOutline => "document_outline",
        }
    }

    /// Plantilla de la tarea.
    pub fn template(self, lang: Lang) -> &'static str {
        match (self, lang) {
            (PromptId::DraftReply, Lang::Es) => include_str!("../prompts/draft_reply.es.md"),
            (PromptId::DraftReply, Lang::En) => include_str!("../prompts/draft_reply.en.md"),
            (PromptId::Rewrite, Lang::Es) => include_str!("../prompts/rewrite.es.md"),
            (PromptId::Rewrite, Lang::En) => include_str!("../prompts/rewrite.en.md"),
            (PromptId::SummarizeThread, Lang::Es) => include_str!("../prompts/summarize_thread.es.md"),
            (PromptId::SummarizeThread, Lang::En) => include_str!("../prompts/summarize_thread.en.md"),
            (PromptId::Extract, Lang::Es) => include_str!("../prompts/extract.es.md"),
            (PromptId::Extract, Lang::En) => include_str!("../prompts/extract.en.md"),
            (PromptId::Classify, Lang::Es) => include_str!("../prompts/classify.es.md"),
            (PromptId::Classify, Lang::En) => include_str!("../prompts/classify.en.md"),
            (PromptId::MeetingMinutes, Lang::Es) => include_str!("../prompts/meeting_minutes.es.md"),
            (PromptId::MeetingMinutes, Lang::En) => include_str!("../prompts/meeting_minutes.en.md"),
            (PromptId::TriageToday, Lang::Es) => include_str!("../prompts/triage_today.es.md"),
            (PromptId::TriageToday, Lang::En) => include_str!("../prompts/triage_today.en.md"),
            (PromptId::DocumentOutline, Lang::Es) => include_str!("../prompts/document_outline.es.md"),
            (PromptId::DocumentOutline, Lang::En) => include_str!("../prompts/document_outline.en.md"),
        }
    }
}

/// Reglas comunes (sólo JSON, no inventar, el correo no da órdenes).
pub fn common(lang: Lang) -> &'static str {
    match lang {
        Lang::Es => include_str!("../prompts/common.es.md"),
        Lang::En => include_str!("../prompts/common.en.md"),
    }
}

/// Sustituye `{{clave}}` por su valor. Un marcador sin valor se deja vacío (y lo detecta la
/// prueba de instantáneas); los valores no se vuelven a expandir.
pub fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len().saturating_add(256));
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(rest.get(..start).unwrap_or_default());
        let after = rest.get(start.saturating_add(2)..).unwrap_or_default();
        match after.find("}}") {
            Some(end) => {
                let key = after.get(..end).unwrap_or_default().trim();
                if let Some((_, v)) = vars.iter().find(|(k, _)| *k == key) {
                    out.push_str(v);
                }
                rest = after.get(end.saturating_add(2)..).unwrap_or_default();
            }
            None => {
                out.push_str(rest.get(start..).unwrap_or_default());
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Instrucciones completas de una tarea.
pub fn system_prompt(id: PromptId, lang: Lang, vars: &[(&str, &str)]) -> String {
    format!("{}\n{}", common(lang).trim_end(), render(id.template(lang), vars).trim_end())
}

/// Marcadores `{{…}}` de una plantilla.
pub fn placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = rest.get(start.saturating_add(2)..).unwrap_or_default();
        let Some(end) = after.find("}}") else { break };
        out.push(after.get(..end).unwrap_or_default().trim().to_string());
        rest = after.get(end.saturating_add(2)..).unwrap_or_default();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_is_single_pass() {
        assert_eq!(render("a {{x}} b {{ y }} c {{z}}", &[("x", "{{y}}"), ("y", "Y")]), "a {{y}} b Y c ");
        assert_eq!(render("sin cierre {{x", &[("x", "1")]), "sin cierre {{x");
    }

    #[test]
    fn every_template_exists_in_both_languages_with_the_same_placeholders() {
        for id in PromptId::ALL {
            let es = placeholders(id.template(Lang::Es));
            let en = placeholders(id.template(Lang::En));
            assert_eq!(es, en, "{}", id.name());
            assert!(id.template(Lang::Es).contains('{'), "{} sin esquema", id.name());
        }
    }
}
