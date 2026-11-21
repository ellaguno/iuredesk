//! Tareas y disponibilidad por EWS (F8.4).
//!
//! - Tareas: la carpeta de tareas del buzón (`t:Task`), con los mismos pasos que los contactos
//!   (`SyncFolderItems` con los ids, `GetItem` con los datos, crear, [`EwsClient::update_item`]).
//! - Disponibilidad (`GetUserAvailability`): los intervalos ocupados de otras personas de la
//!   organización, para el asistente de programación. Se pide en UTC (zona con desfase cero) y
//!   cada persona responde por separado: una que no se puede consultar queda «desconocida».

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::ops::{ids, items_of, t};
use crate::soap::{M, esc, folder_id, item_id, messages, single};
use crate::{Element, EwsClient, EwsError, Result};

/// Elementos por lote de `GetItem`.
pub const BATCH: usize = 50;
/// Personas por consulta de disponibilidad (el límite de Exchange es 100).
pub const MAX_PEOPLE: usize = 100;
/// Días máximos por consulta de disponibilidad (el límite de Exchange es 42).
pub const MAX_DAYS: i64 = 42;

/// Un intervalo de la agenda de otra persona.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Busy {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// `Busy`, `Tentative`, `OOF`, `WorkingElsewhere` (los `Free` y `NoData` no se devuelven).
    pub kind: String,
}

impl EwsClient {
    /// Tareas completas (`t:Task`, cuerpo en texto). Lo que no es tarea y lo que ya no existe se
    /// omite.
    pub async fn get_tasks(&self, ids: &[String]) -> Result<Vec<Element>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(BATCH) {
            let list: String = chunk.iter().map(|i| item_id(i, None)).collect();
            let r = self
                .call(&format!(
                    "<m:GetItem><m:ItemShape><t:BaseShape>AllProperties</t:BaseShape><t:BodyType>Text</t:BodyType></m:ItemShape><m:ItemIds>{list}</m:ItemIds></m:GetItem>"
                ))
                .await?;
            for msg in messages(&r) {
                match msg {
                    Ok(m) => out.extend(items_of(m).into_iter().filter(|e| e.name == "Task").cloned()),
                    Err(e) if e.is_not_found() => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(out)
    }

    /// Una tarea completa, o `None` si ya no existe.
    pub async fn get_task(&self, id: &str) -> Result<Option<Element>> {
        Ok(self.get_tasks(&[id.to_string()]).await?.into_iter().next())
    }

    /// Crea una tarea en `folder` con `inner` (campos de `t:Task` en el orden del esquema).
    pub async fn create_task(&self, folder: &str, inner: &str) -> Result<(String, Option<String>)> {
        let r = self
            .call(&format!(
                "<m:CreateItem><m:SavedItemFolderId>{}</m:SavedItemFolderId><m:Items><t:Task>{inner}</t:Task></m:Items></m:CreateItem>",
                folder_id(folder)
            ))
            .await?;
        items_of(single(&r)?)
            .into_iter()
            .find_map(|i| ids(i, "ItemId"))
            .ok_or_else(|| EwsError::Invalid("CreateItem sin id".into()))
    }

    /// Intervalos ocupados de cada persona de `emails` entre `from` y `to` (hasta [`MAX_PEOPLE`]
    /// personas y [`MAX_DAYS`] días). `None` para quien no se pudo consultar (otra organización,
    /// sin permiso, no existe).
    pub async fn availability(
        &self,
        emails: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Option<Vec<Busy>>>> {
        let emails: Vec<&String> = emails.iter().take(MAX_PEOPLE).collect();
        if emails.is_empty() || to <= from {
            return Ok(Vec::new());
        }
        let r = self.call(&availability_request(&emails, from, to)).await?;
        Ok(parse_availability(&r, emails.len()))
    }
}

/// Zona UTC para `GetUserAvailability`: desfase cero todo el año. Exchange rechaza («The
/// specified time zone isn't valid») una zona cuyos cambios de invierno y verano caen el mismo
/// día, así que van en fechas distintas aunque los dos desfases sean cero.
const UTC_TIME_ZONE: &str = "<t:TimeZone><t:Bias>0</t:Bias><t:StandardTime><t:Bias>0</t:Bias><t:Time>02:00:00</t:Time><t:DayOrder>5</t:DayOrder><t:Month>10</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek></t:StandardTime><t:DaylightTime><t:Bias>0</t:Bias><t:Time>02:00:00</t:Time><t:DayOrder>5</t:DayOrder><t:Month>3</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek></t:DaylightTime></t:TimeZone>";

/// Cuerpo de `GetUserAvailability` (horas en UTC, ventana acotada a [`MAX_DAYS`]).
pub fn availability_request(emails: &[&String], from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    {
        let to = to.min(from + chrono::Duration::days(MAX_DAYS));
        let people: String = emails
            .iter()
            .map(|e| {
                format!(
                    "<t:MailboxData><t:Email><t:Address>{}</t:Address></t:Email><t:AttendeeType>Required</t:AttendeeType><t:ExcludeConflicts>false</t:ExcludeConflicts></t:MailboxData>",
                    esc(e.trim())
                )
            })
            .collect();
        let fmt = |d: DateTime<Utc>| d.format("%Y-%m-%dT%H:%M:%S").to_string();
        format!(
            "<m:GetUserAvailabilityRequest>{UTC_TIME_ZONE}<m:MailboxDataArray>{people}</m:MailboxDataArray><t:FreeBusyViewOptions><t:TimeWindow><t:StartTime>{}</t:StartTime><t:EndTime>{}</t:EndTime></t:TimeWindow><t:MergedFreeBusyIntervalInMinutes>30</t:MergedFreeBusyIntervalInMinutes><t:RequestedView>FreeBusy</t:RequestedView></t:FreeBusyViewOptions></m:GetUserAvailabilityRequest>",
            fmt(from),
            fmt(to)
        )
    }
}

/// Las respuestas de `GetUserAvailability`, en el orden de la petición (`n` personas).
pub fn parse_availability(r: &Element, n: usize) -> Vec<Option<Vec<Busy>>> {
    let list: Vec<&Element> =
        r.child(M, "FreeBusyResponseArray").map(|a| a.children.iter().take(MAX_PEOPLE).collect()).unwrap_or_default();
    let instant = |e: &Element, name: &str| -> Option<DateTime<Utc>> {
        let s = t(e, name)?.text_trim();
        let s = s.trim_end_matches('Z');
        NaiveDateTime::parse_from_str(s.split('.').next().unwrap_or(s), "%Y-%m-%dT%H:%M:%S").ok().map(|n| n.and_utc())
    };
    (0..n)
        .map(|i| {
            let resp = list.get(i)?;
            let ok = resp
                .child(M, "ResponseMessage")
                .is_none_or(|m| m.attr("ResponseClass").is_none_or(|c| !c.eq_ignore_ascii_case("Error")));
            if !ok {
                return None;
            }
            let view = resp.child(M, "FreeBusyView")?;
            let events = t(view, "CalendarEventArray");
            Some(
                events
                    .map(|a| {
                        a.children
                            .iter()
                            .take(2_000)
                            .filter_map(|ev| {
                                let kind = t(ev, "BusyType").map(|b| b.text_trim().to_string()).unwrap_or_default();
                                if matches!(kind.as_str(), "Free" | "NoData") {
                                    return None;
                                }
                                let (start, end) = (instant(ev, "StartTime")?, instant(ev, "EndTime")?);
                                (end > start).then_some(Busy { start, end, kind })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::soap::T;

    /// Regresión (F8.5, Exchange SE real): con los dos cambios de hora el mismo día, Exchange
    /// respondía «The specified time zone isn't valid».
    #[test]
    fn availability_time_zone_has_distinct_transitions() {
        let a = "ana@contoso.com".to_string();
        let from = "2026-11-25T00:00:00Z".parse().unwrap();
        let x = availability_request(&[&a], from, from + chrono::Duration::days(100));
        let part = |tag: &str| {
            x.split(&format!("<t:{tag}>"))
                .nth(1)
                .and_then(|r| r.split(&format!("</t:{tag}>")).next())
                .unwrap()
                .to_string()
        };
        assert_ne!(part("StandardTime"), part("DaylightTime"));
        assert!(
            part("StandardTime").contains("<t:Bias>0</t:Bias>") && part("DaylightTime").contains("<t:Bias>0</t:Bias>")
        );
        assert!(x.contains("<t:EndTime>2027-01-06T00:00:00</t:EndTime>"), "ventana acotada a 42 días: {x}");
        assert!(x.contains("<t:Address>ana@contoso.com</t:Address>"));
    }

    #[test]
    fn availability_per_person_in_order() {
        let x = format!(
            r#"<m:GetUserAvailabilityResponse xmlns:m="{M}" xmlns:t="{T}"><m:FreeBusyResponseArray>
<m:FreeBusyResponse><m:ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage><m:FreeBusyView><t:FreeBusyViewType>FreeBusy</t:FreeBusyViewType><t:CalendarEventArray>
  <t:CalendarEvent><t:StartTime>2026-11-18T15:00:00</t:StartTime><t:EndTime>2026-11-18T16:00:00</t:EndTime><t:BusyType>Busy</t:BusyType></t:CalendarEvent>
  <t:CalendarEvent><t:StartTime>2026-11-18T17:00:00</t:StartTime><t:EndTime>2026-11-18T17:30:00</t:EndTime><t:BusyType>Tentative</t:BusyType></t:CalendarEvent>
  <t:CalendarEvent><t:StartTime>2026-11-18T18:00:00</t:StartTime><t:EndTime>2026-11-18T19:00:00</t:EndTime><t:BusyType>Free</t:BusyType></t:CalendarEvent>
  <t:CalendarEvent><t:StartTime>basura</t:StartTime><t:EndTime>2026-11-18T19:00:00</t:EndTime><t:BusyType>Busy</t:BusyType></t:CalendarEvent>
</t:CalendarEventArray></m:FreeBusyView></m:FreeBusyResponse>
<m:FreeBusyResponse><m:ResponseMessage ResponseClass="Error"><m:MessageText>No existe</m:MessageText><m:ResponseCode>ErrorMailRecipientNotFound</m:ResponseCode></m:ResponseMessage></m:FreeBusyResponse>
<m:FreeBusyResponse><m:ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage><m:FreeBusyView><t:FreeBusyViewType>FreeBusy</t:FreeBusyViewType></m:FreeBusyView></m:FreeBusyResponse>
</m:FreeBusyResponseArray></m:GetUserAvailabilityResponse>"#
        );
        let e = iuredesk_dav::dav::xml::parse(x.as_bytes()).unwrap();
        let r = parse_availability(&e, 4);
        assert_eq!(r.len(), 4);
        let first = r[0].as_ref().unwrap();
        assert_eq!(first.len(), 2, "sin el libre ni el roto");
        assert_eq!(first[0].start.to_rfc3339(), "2026-11-18T15:00:00+00:00");
        assert_eq!(first[1].kind, "Tentative");
        assert_eq!(r[1], None, "no se pudo consultar");
        assert_eq!(r[2], Some(Vec::new()), "libre todo el rato");
        assert_eq!(r[3], None, "sin respuesta");
    }
}
