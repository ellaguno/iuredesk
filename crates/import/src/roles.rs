//! Función de una carpeta (Bandeja de entrada, Enviados…) a partir de su nombre, en varios idiomas.
//!
//! Los programas de origen no siempre marcan las carpetas especiales: Thunderbird las nombra en el
//! idioma de la instalación («Enviados», «Papelera»), Outlook según su idioma («Elementos
//! enviados», «Bandeja de entrada»), Apple Mail en inglés en disco («Sent Messages»). Solo las
//! carpetas de primer nivel reciben función; «Proyectos/Enviados» sigue siendo una carpeta normal.

use iuredesk_model::FolderRole;

/// Nombres conocidos por función (ya normalizados: minúsculas, sin acentos, espacios simples).
const NAMES: &[(FolderRole, &[&str])] = &[
    (
        FolderRole::Inbox,
        &[
            "inbox",
            "bandeja de entrada",
            "entrada",
            "recibidos",
            "correo entrante",
            "posteingang",
            "boite de reception",
            "caixa de entrada",
            "posta in arrivo",
            "postvak in",
            "inkorgen",
        ],
    ),
    (
        FolderRole::Sent,
        &[
            "sent",
            "sent items",
            "sent mail",
            "sent messages",
            "sent-mail",
            "enviados",
            "enviado",
            "elementos enviados",
            "correo enviado",
            "mensajes enviados",
            "gesendet",
            "gesendete elemente",
            "gesendete objekte",
            "envoyes",
            "elements envoyes",
            "messages envoyes",
            "posta inviata",
            "inviati",
            "itens enviados",
            "verzonden items",
            "skickat",
        ],
    ),
    (
        FolderRole::Drafts,
        &["drafts", "draft", "borradores", "borrador", "entwurfe", "brouillons", "bozze", "rascunhos", "concepten"],
    ),
    (
        FolderRole::Trash,
        &[
            "trash",
            "deleted",
            "deleted items",
            "deleted messages",
            "bin",
            "papelera",
            "elementos eliminados",
            "eliminados",
            "mensajes eliminados",
            "papierkorb",
            "geloschte elemente",
            "corbeille",
            "elements supprimes",
            "cestino",
            "posta eliminata",
            "lixeira",
            "lixo",
            "itens excluidos",
            "verwijderde items",
            "prullenbak",
        ],
    ),
    (
        FolderRole::Junk,
        &[
            "junk",
            "junk e-mail",
            "junk email",
            "junk mail",
            "spam",
            "bulk mail",
            "correo no deseado",
            "no deseado",
            "correo basura",
            "junk-e-mail",
            "courrier indesirable",
            "indesirables",
            "posta indesiderata",
            "lixo eletronico",
            "ongewenste e-mail",
        ],
    ),
    (
        FolderRole::Archive,
        &["archive", "archives", "archivo", "archivos", "archivado", "archiv", "archivio", "arquivo", "archief"],
    ),
];

/// Quita acentos comunes y normaliza espacios y mayúsculas.
pub(crate) fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut space = false;
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        let c = match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            '_' => ' ',
            c => c,
        };
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

/// Función de una carpeta de primer nivel por su nombre; `Custom` si no se reconoce.
pub fn role_for_name(name: &str) -> FolderRole {
    let n = normalize(name);
    // `[Gmail]/Enviados`, `INBOX.Sent` (Maildir++): el último segmento es el que cuenta.
    let n = n.rsplit(['/', '.']).next().unwrap_or(&n).trim().to_string();
    for (role, names) in NAMES {
        if names.contains(&n.as_str()) {
            return *role;
        }
    }
    FolderRole::Custom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localized_names() {
        assert_eq!(role_for_name("Bandeja de entrada"), FolderRole::Inbox);
        assert_eq!(role_for_name("INBOX"), FolderRole::Inbox);
        assert_eq!(role_for_name("Enviados"), FolderRole::Sent);
        assert_eq!(role_for_name("Elementos enviados"), FolderRole::Sent);
        assert_eq!(role_for_name("Sent Messages"), FolderRole::Sent);
        assert_eq!(role_for_name("Papelera"), FolderRole::Trash);
        assert_eq!(role_for_name("Elementos eliminados"), FolderRole::Trash);
        assert_eq!(role_for_name("Deleted Items"), FolderRole::Trash);
        assert_eq!(role_for_name("Correo no deseado"), FolderRole::Junk);
        assert_eq!(role_for_name("Junk E-mail"), FolderRole::Junk);
        assert_eq!(role_for_name("Borradores"), FolderRole::Drafts);
        assert_eq!(role_for_name("Entwürfe"), FolderRole::Drafts);
        assert_eq!(role_for_name("Éléments envoyés"), FolderRole::Sent);
        assert_eq!(role_for_name("Archivo"), FolderRole::Archive);
        assert_eq!(role_for_name("[Gmail]/Enviados"), FolderRole::Sent);
        assert_eq!(role_for_name(".Sent"), FolderRole::Sent);
        assert_eq!(role_for_name("Proyectos"), FolderRole::Custom);
        assert_eq!(role_for_name(""), FolderRole::Custom);
    }
}
