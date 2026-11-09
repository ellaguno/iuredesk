//! Sobre SOAP de EWS: envoltura de la petición, escape de texto y lectura de la respuesta
//! (`soap:Fault` y los `ResponseMessage` de cada elemento).

use crate::{Element, EwsError, Result};

/// `soap:` (SOAP 1.1).
pub const SOAP: &str = "http://schemas.xmlsoap.org/soap/envelope/";
/// `m:` (mensajes de EWS).
pub const M: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";
/// `t:` (tipos de EWS).
pub const T: &str = "http://schemas.microsoft.com/exchange/services/2006/types";
/// Versión de esquema que se pide (la entienden Exchange 2013 SP1 y posteriores).
pub const SERVER_VERSION: &str = "Exchange2013_SP1";

/// Escapa texto o un valor de atributo.
pub fn esc(s: &str) -> String {
    quick_xml::escape::escape(s).into_owned()
}

/// Petición completa con `body` dentro de `soap:Body`.
pub fn envelope(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><soap:Envelope xmlns:soap="{SOAP}" xmlns:t="{T}" xmlns:m="{M}"><soap:Header><t:RequestServerVersion Version="{SERVER_VERSION}"/></soap:Header><soap:Body>{body}</soap:Body></soap:Envelope>"#
    )
}

/// Texto acotado.
pub(crate) fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// `soap:Fault` → (código, mensaje). El código es el `ResponseCode` del detalle si lo hay
/// (`ErrorServerBusy`), si no el `faultcode`.
pub fn fault(root: &Element) -> Option<(String, String)> {
    let body = root.child(SOAP, "Body")?;
    let f = body.children.iter().find(|c| c.name == "Fault")?;
    fn find<'a>(e: &'a Element, name: &str) -> Option<&'a Element> {
        if e.name == name {
            return Some(e);
        }
        e.children.iter().find_map(|c| find(c, name))
    }
    let code = find(f, "ResponseCode")
        .or_else(|| f.children.iter().find(|c| c.name == "faultcode"))
        .map(|c| clip(c.text_trim(), 200))
        .unwrap_or_default();
    let message = find(f, "Message")
        .or_else(|| f.children.iter().find(|c| c.name == "faultstring"))
        .map(|c| clip(c.text_trim(), 500))
        .unwrap_or_default();
    Some((code, message))
}

/// El elemento de respuesta (`m:GetItemResponse`…) dentro de `soap:Body`.
pub fn body_response(root: Element) -> Result<Element> {
    if !root.is(SOAP, "Envelope") {
        return Err(EwsError::Invalid(format!("se esperaba soap:Envelope y llegó {}", root.name)));
    }
    root.children
        .into_iter()
        .find(|c| c.is(SOAP, "Body"))
        .and_then(|b| b.children.into_iter().next())
        .ok_or_else(|| EwsError::Invalid("soap:Body vacío".into()))
}

/// Los `ResponseMessage` de una respuesta, cada uno con su resultado: `Ok` con el mensaje si
/// `ResponseClass` es `Success` o `Warning`, `Err` con su `ResponseCode` si es `Error`.
pub fn messages(response: &Element) -> Vec<Result<&Element>> {
    let Some(list) = response.child(M, "ResponseMessages") else { return Vec::new() };
    list.children
        .iter()
        .map(|m| match m.attr("ResponseClass") {
            Some("Error") => Err(EwsError::Soap {
                code: m.child(M, "ResponseCode").map(|c| clip(c.text_trim(), 200)).unwrap_or_default(),
                message: m.child(M, "MessageText").map(|c| clip(c.text_trim(), 500)).unwrap_or_default(),
            }),
            _ => Ok(m),
        })
        .collect()
}

/// El único `ResponseMessage` de una petición de un solo elemento.
pub fn single(response: &Element) -> Result<&Element> {
    messages(response).into_iter().next().unwrap_or_else(|| Err(EwsError::Invalid("respuesta sin mensajes".into())))
}

/// `<t:FolderId Id=…/>` o `<t:DistinguishedFolderId Id=…/>` según el id (los ids reales de EWS
/// son base64 largos; los distinguidos son palabras: `inbox`, `msgfolderroot`…).
pub fn folder_id(id: &str) -> String {
    if is_distinguished(id) {
        format!(r#"<t:DistinguishedFolderId Id="{}"/>"#, esc(id))
    } else {
        format!(r#"<t:FolderId Id="{}"/>"#, esc(id))
    }
}

/// Carpetas con nombre de EWS que se usan aquí.
pub const DISTINGUISHED: &[&str] =
    &["inbox", "sentitems", "drafts", "deleteditems", "junkemail", "archive", "msgfolderroot", "outbox"];

/// ¿Es un id distinguido?
pub fn is_distinguished(id: &str) -> bool {
    DISTINGUISHED.contains(&id)
}

/// `<t:ItemId Id=… [ChangeKey=…]/>`.
pub fn item_id(id: &str, change_key: Option<&str>) -> String {
    match change_key.filter(|c| !c.is_empty()) {
        Some(ck) => format!(r#"<t:ItemId Id="{}" ChangeKey="{}"/>"#, esc(id), esc(ck)),
        None => format!(r#"<t:ItemId Id="{}"/>"#, esc(id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Element {
        iuredesk_dav::dav::xml::parse(s.as_bytes()).unwrap()
    }

    #[test]
    fn envelope_escapes_and_reads_messages() {
        let e = envelope(&format!("<m:GetItem>{}</m:GetItem>", item_id("a<b&\"c", None)));
        assert!(e.contains(r#"Id="a&lt;b&amp;&quot;c""#), "{e}");
        let root = parse(&format!(
            r#"<s:Envelope xmlns:s="{SOAP}"><s:Body><m:GetItemResponse xmlns:m="{M}"><m:ResponseMessages>
<m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:GetItemResponseMessage>
<m:GetItemResponseMessage ResponseClass="Error"><m:MessageText>No está</m:MessageText><m:ResponseCode>ErrorItemNotFound</m:ResponseCode></m:GetItemResponseMessage>
</m:ResponseMessages></m:GetItemResponse></s:Body></s:Envelope>"#
        ));
        assert!(fault(&root).is_none());
        let r = body_response(root).unwrap();
        let m = messages(&r);
        assert_eq!(m.len(), 2);
        assert!(m[0].is_ok());
        assert!(m[1].as_ref().is_err_and(EwsError::is_not_found));
    }

    #[test]
    fn server_busy_fault() {
        let root = parse(&format!(
            r#"<s:Envelope xmlns:s="{SOAP}"><s:Body><s:Fault><faultcode>a:ErrorServerBusy</faultcode><faultstring>ocupado</faultstring>
<detail><e:ResponseCode xmlns:e="x">ErrorServerBusy</e:ResponseCode><e:MessageXml xmlns:e="x"><t:Value xmlns:t="{T}" Name="BackOffMilliseconds">250</t:Value></e:MessageXml></detail>
</s:Fault></s:Body></s:Envelope>"#
        ));
        assert_eq!(fault(&root).unwrap().0, "ErrorServerBusy");
        assert_eq!(crate::back_off(&root), Some(250));
    }

    #[test]
    fn folder_ids() {
        assert_eq!(folder_id("inbox"), r#"<t:DistinguishedFolderId Id="inbox"/>"#);
        assert!(folder_id("AAMkADk=").starts_with("<t:FolderId "));
    }
}
