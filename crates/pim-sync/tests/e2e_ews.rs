//! Extremo a extremo contra un Exchange (EWS) falso (F8.4): calendarios por `GetFolder`,
//! apariciones por `CalendarView` (una serie expandida por el servidor, con su maestro por
//! `RecurringMasterItemId`), datos por `GetItem`, edición con `UpdateItem` solo del campo cambiado
//! y su `ChangeKey`, conflicto con fusión a tres bandas, alta con `CreateItem`, respuesta con
//! `AcceptItem` (sin correo iMIP), borrado y lo borrado en el servidor.

#[path = "../../dav/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Req, Resp, between, serve};
use iuredesk_accounts::{MemoryStore, SecretPurpose, SecretStore};
use iuredesk_model::account::{Account, AccountKind, AuthMethod, Protocol, Security, ServerConfig};
use iuredesk_model::{Event, EventTime, PartStat};
use iuredesk_pim_sync::{ConflictResolution, PimAccount, PimEvent, PimSource, PimSyncEngine, PimSyncOptions};
use iuredesk_store::Store;

const T: &str = "http://schemas.microsoft.com/exchange/services/2006/types";
const M: &str = "http://schemas.microsoft.com/exchange/services/2006/messages";

#[derive(Clone)]
struct Item {
    subject: String,
    location: String,
    day: u32,
    kind: &'static str,
    uid: String,
    ck: u32,
    attendees: bool,
}

#[derive(Default)]
struct X {
    items: BTreeMap<String, Item>,
    seq: u32,
    /// Cuerpos de las peticiones de escritura (para mirar qué se mandó).
    writes: Vec<String>,
    /// Elementos pedidos con `GetItem`.
    fetched: usize,
    /// Contactos: id → (nombre, correo, puesto, change key).
    contacts: BTreeMap<String, (String, String, String, u32)>,
    /// Registro de cambios de la carpeta de contactos (lo que da `SyncFolderItems`).
    log: Vec<(&'static str, String)>,
    /// El servidor olvidó los estados de sincronización anteriores a este índice.
    forgot_before: usize,
    /// Tareas: id → (asunto, estado, change key), y su registro de cambios.
    tasks: BTreeMap<String, (String, String, u32)>,
    tlog: Vec<(&'static str, String)>,
}

impl X {
    fn add_contact(&mut self, id: &str, name: &str, email: &str, title: &str) {
        self.seq += 1;
        self.contacts.insert(id.into(), (name.into(), email.into(), title.into(), self.seq));
        self.log.push(("Create", id.into()));
    }

    fn add_task(&mut self, id: &str, subject: &str) {
        self.seq += 1;
        self.tasks.insert(id.into(), (subject.into(), "NotStarted".into(), self.seq));
        self.tlog.push(("Create", id.into()));
    }

    fn task_xml(&self, id: &str) -> String {
        let Some((subject, status, ck)) = self.tasks.get(id) else { return String::new() };
        format!(
            r#"<t:Task><t:ItemId Id="{id}" ChangeKey="ck{ck}"/><t:Subject>{subject}</t:Subject><t:Importance>High</t:Importance><t:DueDate>2026-11-20T06:00:00Z</t:DueDate><t:Status>{status}</t:Status></t:Task>"#
        )
    }

    fn contact_xml(&self, id: &str) -> String {
        let Some((name, email, title, ck)) = self.contacts.get(id) else { return String::new() };
        format!(
            r#"<t:Contact><t:ItemId Id="{id}" ChangeKey="ck{ck}"/><t:DisplayName>{name}</t:DisplayName><t:EmailAddresses><t:Entry Key="EmailAddress1">{email}</t:Entry></t:EmailAddresses><t:PhoneNumbers><t:Entry Key="BusinessFax">+52 81 0000</t:Entry></t:PhoneNumbers><t:JobTitle>{title}</t:JobTitle></t:Contact>"#
        )
    }

    fn add(&mut self, id: &str, subject: &str, day: u32, kind: &'static str, uid: &str) {
        self.seq += 1;
        let it = Item {
            subject: subject.into(),
            location: String::new(),
            day,
            kind,
            uid: uid.into(),
            ck: self.seq,
            attendees: kind != "Single",
        };
        self.items.insert(id.into(), it);
    }

    fn bump(&mut self, id: &str) {
        self.seq += 1;
        let seq = self.seq;
        if let Some(it) = self.items.get_mut(id) {
            it.ck = seq;
        }
    }

    fn xml(&self, id: &str) -> String {
        let Some(it) = self.items.get(id) else { return String::new() };
        let att = if it.attendees {
            "<t:IsMeeting>true</t:IsMeeting><t:MyResponseType>NoResponseReceived</t:MyResponseType><t:Organizer><t:Mailbox><t:Name>Bea</t:Name><t:EmailAddress>bea@contoso.com</t:EmailAddress></t:Mailbox></t:Organizer><t:RequiredAttendees><t:Attendee><t:Mailbox><t:EmailAddress>ana@contoso.com</t:EmailAddress></t:Mailbox><t:ResponseType>Unknown</t:ResponseType></t:Attendee></t:RequiredAttendees>"
        } else {
            "<t:IsMeeting>false</t:IsMeeting><t:MyResponseType>Organizer</t:MyResponseType>"
        };
        let rec = if it.kind == "RecurringMaster" {
            "<t:Recurrence><t:WeeklyRecurrence><t:Interval>1</t:Interval><t:DaysOfWeek>Monday</t:DaysOfWeek><t:FirstDayOfWeek>Sunday</t:FirstDayOfWeek></t:WeeklyRecurrence><t:NumberedRecurrence><t:StartDate>2026-11-16</t:StartDate><t:NumberOfOccurrences>4</t:NumberOfOccurrences></t:NumberedRecurrence></t:Recurrence>"
        } else {
            ""
        };
        format!(
            r#"<t:CalendarItem><t:ItemId Id="{id}" ChangeKey="ck{}"/><t:Subject>{}</t:Subject><t:Body BodyType="Text"></t:Body><t:ReminderIsSet>false</t:ReminderIsSet><t:UID>{}</t:UID><t:Start>2026-11-{:02}T16:00:00Z</t:Start><t:End>2026-11-{:02}T17:00:00Z</t:End><t:IsAllDayEvent>false</t:IsAllDayEvent><t:LegacyFreeBusyStatus>Busy</t:LegacyFreeBusyStatus><t:Location>{}</t:Location>{att}<t:CalendarItemType>{}</t:CalendarItemType>{rec}<t:StartTimeZone Id="Central Standard Time (Mexico)"/></t:CalendarItem>"#,
            it.ck, it.subject, it.uid, it.day, it.day, it.location, it.kind
        )
    }
}

fn envelope(inner: &str) -> Resp {
    Resp::xml(
        200,
        format!(
            r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body>{inner}</s:Body></s:Envelope>"#
        ),
    )
}

fn response(op: &str, msgs: &str) -> Resp {
    envelope(&format!(
        r#"<m:{op}Response xmlns:m="{M}" xmlns:t="{T}"><m:ResponseMessages>{msgs}</m:ResponseMessages></m:{op}Response>"#
    ))
}

/// Respuesta sin `ResponseMessages` (`GetUserAvailability` tiene su propia forma).
fn response_plain(name: &str, inner: &str) -> Resp {
    envelope(&format!(r#"<m:{name} xmlns:m="{M}" xmlns:t="{T}">{inner}</m:{name}>"#))
}

fn ok(op: &str, inner: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>{inner}</m:{op}ResponseMessage>"#
    )
}

fn err(op: &str, code: &str) -> String {
    format!(
        r#"<m:{op}ResponseMessage ResponseClass="Error"><m:MessageText>x</m:MessageText><m:ResponseCode>{code}</m:ResponseCode></m:{op}ResponseMessage>"#
    )
}

/// Todos los `Id="…"` de las etiquetas `tag` del cuerpo.
fn ids(body: &str, tag: &str) -> Vec<String> {
    body.split(&format!("<t:{tag} Id=\"")).skip(1).filter_map(|s| s.split('"').next()).map(str::to_string).collect()
}

fn handle(st: &Mutex<X>, req: Req) -> Resp {
    let mut x = st.lock().unwrap();
    let b = req.body.clone();
    if req.header("Authorization").unwrap_or_default() != "Basic YW5hQGNvbnRvc28uY29tOnNlY3JldG8=" {
        return Resp::new(401, "").with("WWW-Authenticate", "Basic realm=\"x\"");
    }
    if b.contains("<m:GetFolder>") {
        assert!(b.contains(r#"<t:DistinguishedFolderId Id="calendar"/>"#));
        return response(
            "GetFolder",
            &ok(
                "GetFolder",
                r#"<m:Folders><t:CalendarFolder><t:FolderId Id="F-cal" ChangeKey="f1"/><t:DisplayName>Calendario</t:DisplayName><t:FolderClass>IPF.Appointment</t:FolderClass><t:ChildFolderCount>0</t:ChildFolderCount></t:CalendarFolder></m:Folders>"#,
            ),
        );
    }
    if b.contains("<m:CalendarView") {
        let (start, end) = (between(&b, "StartDate=\"", "\"").unwrap(), between(&b, "EndDate=\"", "\"").unwrap());
        let in_range = |d: u32| {
            let t = format!("2026-11-{d:02}T16:00:00Z");
            t.as_str() >= start && t.as_str() < end
        };
        let list: String = x
            .items
            .iter()
            .filter(|(_, it)| in_range(it.day) && it.kind != "RecurringMaster")
            .map(|(id, it)| {
                format!(
                    r#"<t:CalendarItem><t:ItemId Id="{id}" ChangeKey="ck{}"/><t:CalendarItemType>{}</t:CalendarItemType></t:CalendarItem>"#,
                    it.ck, it.kind
                )
            })
            .collect();
        return response(
            "FindItem",
            &ok(
                "FindItem",
                &format!(r#"<m:RootFolder IncludesLastItemInRange="true"><t:Items>{list}</t:Items></m:RootFolder>"#),
            ),
        );
    }
    if b.contains("<m:SyncFolderItems>") {
        let tasks = b.contains(r#"<t:DistinguishedFolderId Id="tasks"/>"#);
        assert!(tasks || b.contains(r#"<t:DistinguishedFolderId Id="contacts"/>"#), "{b}");
        let log = if tasks { x.tlog.clone() } else { x.log.clone() };
        let from = match between(&b, "<m:SyncState>", "</m:SyncState>") {
            None => 0,
            Some(s) => match s.strip_prefix('s').and_then(|n| n.parse::<usize>().ok()) {
                Some(n) if n >= x.forgot_before => n,
                _ => return response("SyncFolderItems", &err("SyncFolderItems", "ErrorInvalidSyncStateData")),
            },
        };
        let changes: String = log
            .iter()
            .skip(from)
            .map(|(kind, id)| match *kind {
                "Delete" => format!(r#"<t:Delete><t:ItemId Id="{id}"/></t:Delete>"#),
                k => format!(r#"<t:{k}><t:Contact><t:ItemId Id="{id}" ChangeKey="x"/></t:Contact></t:{k}>"#),
            })
            .collect();
        let state = log.len();
        return response(
            "SyncFolderItems",
            &ok(
                "SyncFolderItems",
                &format!(
                    "<m:SyncState>s{state}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes>{changes}</m:Changes>"
                ),
            ),
        );
    }
    if b.contains("<m:ResolveNames") {
        assert!(b.contains("<m:UnresolvedEntry>ana</m:UnresolvedEntry>"), "{b}");
        return response(
            "ResolveNames",
            r#"<m:ResolveNamesResponseMessage ResponseClass="Warning"><m:ResponseCode>ErrorNameResolutionMultipleResults</m:ResponseCode><m:ResolutionSet><t:Resolution><t:Mailbox><t:Name>Ana Pérez</t:Name><t:EmailAddress>ana.perez@contoso.com</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox></t:Resolution><t:Resolution><t:Mailbox><t:Name>Anabel Ríos</t:Name><t:EmailAddress>anabel@contoso.com</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox></t:Resolution></m:ResolutionSet></m:ResolveNamesResponseMessage>"#,
        );
    }
    if b.contains("<t:RecurringMasterItemId") {
        return response(
            "GetItem",
            &ok("GetItem", r#"<m:Items><t:CalendarItem><t:ItemId Id="m1" ChangeKey="cm"/></t:CalendarItem></m:Items>"#),
        );
    }
    if b.contains("<m:GetItem>") {
        x.fetched += ids(&b, "ItemId").len();
        let msgs: String = ids(&b, "ItemId")
            .iter()
            .map(|id| {
                if x.items.contains_key(id) {
                    ok("GetItem", &format!("<m:Items>{}</m:Items>", x.xml(id)))
                } else if x.tasks.contains_key(id) {
                    ok("GetItem", &format!("<m:Items>{}</m:Items>", x.task_xml(id)))
                } else if x.contacts.contains_key(id) {
                    ok("GetItem", &format!("<m:Items>{}</m:Items>", x.contact_xml(id)))
                } else {
                    err("GetItem", "ErrorItemNotFound")
                }
            })
            .collect();
        return response("GetItem", &msgs);
    }
    x.writes.push(b.clone());
    if b.contains("<m:UpdateItem") && b.contains("<t:Task>") {
        let id = ids(&b, "ItemId").remove(0);
        let ck = between(&b, "ChangeKey=\"", "\"").map(str::to_string);
        let Some(cur) = x.tasks.get(&id).cloned() else {
            return response("UpdateItem", &err("UpdateItem", "ErrorItemNotFound"));
        };
        if ck.is_some_and(|c| c != format!("ck{}", cur.2)) {
            return response("UpdateItem", &err("UpdateItem", "ErrorIrresolvableConflict"));
        }
        x.seq += 1;
        let seq = x.seq;
        if let Some(t) = x.tasks.get_mut(&id) {
            if let Some(st) = between(&b, "<t:Status>", "</t:Status>") {
                t.1 = st.to_string();
            }
            t.2 = seq;
        }
        x.tlog.push(("Update", id.clone()));
        return response(
            "UpdateItem",
            &ok(
                "UpdateItem",
                &format!(r#"<m:Items><t:Task><t:ItemId Id="{id}" ChangeKey="ck{seq}"/></t:Task></m:Items>"#),
            ),
        );
    }
    if b.contains("<m:GetUserAvailabilityRequest>") {
        assert!(
            b.to_lowercase().contains("<t:address>bea@contoso.com</t:address>") && b.contains("<t:Bias>0</t:Bias>"),
            "{b}"
        );
        return response_plain(
            "GetUserAvailabilityResponse",
            r#"<m:FreeBusyResponseArray><m:FreeBusyResponse><m:ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage><m:FreeBusyView><t:FreeBusyViewType>FreeBusy</t:FreeBusyViewType><t:CalendarEventArray><t:CalendarEvent><t:StartTime>2026-11-25T15:00:00</t:StartTime><t:EndTime>2026-11-25T16:00:00</t:EndTime><t:BusyType>Busy</t:BusyType></t:CalendarEvent><t:CalendarEvent><t:StartTime>2026-11-25T18:00:00</t:StartTime><t:EndTime>2026-11-25T19:00:00</t:EndTime><t:BusyType>Tentative</t:BusyType></t:CalendarEvent></t:CalendarEventArray></m:FreeBusyView></m:FreeBusyResponse><m:FreeBusyResponse><m:ResponseMessage ResponseClass="Error"><m:MessageText>x</m:MessageText><m:ResponseCode>ErrorMailRecipientNotFound</m:ResponseCode></m:ResponseMessage></m:FreeBusyResponse></m:FreeBusyResponseArray>"#,
        );
    }
    if b.contains("<m:UpdateItem") && b.contains("<t:Contact>") {
        let id = ids(&b, "ItemId").remove(0);
        let ck = between(&b, "ChangeKey=\"", "\"").map(str::to_string);
        let Some(cur) = x.contacts.get(&id).cloned() else {
            return response("UpdateItem", &err("UpdateItem", "ErrorItemNotFound"));
        };
        if ck.is_some_and(|c| c != format!("ck{}", cur.3)) {
            return response("UpdateItem", &err("UpdateItem", "ErrorIrresolvableConflict"));
        }
        x.seq += 1;
        let seq = x.seq;
        if let Some(c) = x.contacts.get_mut(&id) {
            if let Some(t) = between(&b, "<t:JobTitle>", "</t:JobTitle>") {
                c.2 = t.to_string();
            }
            c.3 = seq;
        }
        x.log.push(("Update", id.clone()));
        return response(
            "UpdateItem",
            &ok(
                "UpdateItem",
                &format!(r#"<m:Items><t:Contact><t:ItemId Id="{id}" ChangeKey="ck{seq}"/></t:Contact></m:Items>"#),
            ),
        );
    }
    if b.contains("<m:UpdateItem") {
        let id = ids(&b, "ItemId").remove(0);
        let ck = between(&b, "ChangeKey=\"", "\"").map(str::to_string);
        let Some(cur) = x.items.get(&id).cloned() else {
            return response("UpdateItem", &err("UpdateItem", "ErrorItemNotFound"));
        };
        if ck.is_some_and(|c| c != format!("ck{}", cur.ck)) {
            return response("UpdateItem", &err("UpdateItem", "ErrorIrresolvableConflict"));
        }
        // Cambiar el maestro cambia todas las apariciones de la serie.
        let targets: Vec<String> = if cur.kind == "RecurringMaster" {
            x.items.iter().filter(|(_, i)| i.uid == cur.uid).map(|(k, _)| k.clone()).collect()
        } else {
            vec![id.clone()]
        };
        for t in &targets {
            let Some(it) = x.items.get_mut(t) else { continue };
            if let Some(s) = between(&b, "<t:Subject>", "</t:Subject>") {
                it.subject = s.to_string();
            }
            if let Some(l) = between(&b, "<t:Location>", "</t:Location>") {
                it.location = l.to_string();
            }
        }
        for t in &targets {
            x.bump(t);
        }
        let ck = x.items.get(&id).map(|i| i.ck).unwrap_or_default();
        return response(
            "UpdateItem",
            &ok(
                "UpdateItem",
                &format!(
                    r#"<m:Items><t:CalendarItem><t:ItemId Id="{id}" ChangeKey="ck{ck}"/></t:CalendarItem></m:Items>"#
                ),
            ),
        );
    }
    if b.contains("<t:AcceptItem>") {
        return response("CreateItem", &ok("CreateItem", "<m:Items/>"));
    }
    if b.contains("<m:CreateItem") && b.contains("<t:Task>") {
        let subject = between(&b, "<t:Subject>", "</t:Subject>").unwrap_or_default().to_string();
        assert!(b.contains(r#"<t:DistinguishedFolderId Id="tasks"/>"#), "{b}");
        x.add_task("tnew", &subject);
        return response(
            "CreateItem",
            &ok("CreateItem", r#"<m:Items><t:Task><t:ItemId Id="tnew" ChangeKey="x"/></t:Task></m:Items>"#),
        );
    }
    if b.contains("<m:CreateItem") && b.contains("<t:Contact>") {
        let name = between(&b, "<t:DisplayName>", "</t:DisplayName>").unwrap_or_default().to_string();
        let email = between(&b, r#"<t:Entry Key="EmailAddress1">"#, "</t:Entry>").unwrap_or_default().to_string();
        x.add_contact("cnew", &name, &email, "");
        return response(
            "CreateItem",
            &ok("CreateItem", r#"<m:Items><t:Contact><t:ItemId Id="cnew" ChangeKey="x"/></t:Contact></m:Items>"#),
        );
    }
    if b.contains("<m:CreateItem") {
        let subject = between(&b, "<t:Subject>", "</t:Subject>").unwrap_or_default().to_string();
        let day: u32 = between(&b, "<t:Start>2026-11-", "T").and_then(|d| d.parse().ok()).unwrap_or(1);
        x.add("new1", &subject, day, "Single", "uid-new1");
        return response(
            "CreateItem",
            &ok(
                "CreateItem",
                r#"<m:Items><t:CalendarItem><t:ItemId Id="new1" ChangeKey="x"/></t:CalendarItem></m:Items>"#,
            ),
        );
    }
    if b.contains("<m:DeleteItem") {
        for id in ids(&b, "ItemId") {
            x.items.remove(&id);
            if x.contacts.remove(&id).is_some() {
                x.log.push(("Delete", id.clone()));
            }
            if x.tasks.remove(&id).is_some() {
                x.tlog.push(("Delete", id));
            }
        }
        return response("DeleteItem", &ok("DeleteItem", ""));
    }
    Resp::new(500, "inesperado")
}

fn account(url: &str) -> Account {
    let srv = |p| ServerConfig {
        protocol: p,
        host: url.into(),
        port: 443,
        security: Security::Tls,
        auth: AuthMethod::Password,
        username: "ana@contoso.com".into(),
    };
    Account {
        id: "ana-ex".into(),
        display_name: "Ana".into(),
        email: "ana@contoso.com".into(),
        kind: AccountKind::Exchange,
        incoming: srv(Protocol::Ews),
        outgoing: srv(Protocol::Ews),
        caldav_url: None,
        carddav_url: None,
        identities: vec![],
        shared_of: None,
    }
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<PimEvent>) -> Vec<PimEvent> {
    let mut v = Vec::new();
    while let Ok(e) = rx.try_recv() {
        v.push(e);
    }
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ews_calendar_sync_edit_conflict_create_rsvp_delete() {
    let state = Arc::new(Mutex::new(X::default()));
    {
        let mut x = state.lock().unwrap();
        x.add("e1", "Revisión", 25, "Single", "uid-e1");
        x.add("e2", "Comida", 26, "Single", "uid-e2");
        // Dos apariciones de una serie (el maestro m1 no viene en la vista).
        x.add("o1", "Junta semanal", 23, "Occurrence", "uid-serie");
        x.add("o2", "Junta semanal", 30, "Occurrence", "uid-serie");
    }
    let h = Arc::clone(&state);
    let base = serve(move |req| handle(&h, req)).await;
    let url = format!("{base}/EWS/Exchange.asmx");

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("ana-ex", SecretPurpose::Incoming, "secreto").unwrap();
    let pim = PimAccount::from_account(&account(&url)).unwrap();
    assert_eq!(pim.source, PimSource::Ews { url: url.clone() });
    let opts = PimSyncOptions {
        poll_interval: Duration::from_secs(3600),
        sync_on_start: false,
        user_tz: "America/Mexico_City".parse().unwrap(),
        // Ventana que incluye noviembre de 2026 sin depender de la fecha de hoy.
        graph_past_days: 3_650,
        graph_future_days: 3_650,
        ..Default::default()
    };
    let handle = PimSyncEngine::start(Arc::clone(&store), vec![pim], None, secrets, opts);
    let mut events = handle.events();
    handle.sync_now_wait("ana-ex").await.unwrap();

    // --- primera pasada
    let cal = store.calendars().unwrap().remove(0);
    assert_eq!((cal.name.as_str(), cal.remote_path.as_str()), ("Calendario", "F-cal"));
    let caps = handle.calendar_caps(cal.id).unwrap();
    assert!(caps.server_invites && caps.meeting.is_none() && !caps.exceptions);
    let mut evs = store.events_in_calendar(cal.id).unwrap();
    evs.sort_by_key(|e| e.start.to_utc());
    assert_eq!(
        evs.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(),
        ["Junta semanal", "Revisión", "Comida", "Junta semanal"]
    );
    let EventTime::DateTime { tzid, .. } = &evs[1].start else { panic!("hora") };
    assert_eq!(tzid.as_deref(), Some("America/Mexico_City"));
    // Las apariciones conocen su maestro (para «toda la serie»).
    let occ = evs[0].clone();
    assert_eq!(iuredesk_pim_sync::ops::graph_series_master(&store, occ.id).as_deref(), Some("m1"));
    assert_eq!(iuredesk_pim_sync::ops::graph_series_master(&store, evs[1].id), None);

    // --- sin cambios en el servidor: no se vuelve a pedir nada
    let fetched = |s: &Mutex<X>| s.lock().unwrap().fetched;
    assert_eq!(fetched(&state), 4);
    handle.sync_now_wait("ana-ex").await.unwrap();
    assert_eq!(fetched(&state), 4, "mismo ChangeKey: no se piden de nuevo");
    assert!(state.lock().unwrap().writes.is_empty());

    // --- edición local: UpdateItem solo con el título y el ChangeKey
    let mut e1 = evs[1].clone();
    e1.summary = "Revisión trimestral".into();
    handle.save_event(&e1).unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    {
        let x = state.lock().unwrap();
        let w = x.writes.last().unwrap();
        assert!(w.contains(r#"FieldURI="item:Subject""#) && !w.contains("calendar:Start"), "{w}");
        assert!(w.contains(r#"ConflictResolution="NeverOverwrite""#) && w.contains("ChangeKey=\"ck"));
        assert!(w.contains(r#"SendMeetingInvitationsOrCancellations="SendToNone""#), "cita sin asistentes");
        assert_eq!(x.items["e1"].subject, "Revisión trimestral");
    }

    // --- conflicto: el servidor cambió el lugar, nosotros el título → fusión
    let _ = drain(&mut events);
    {
        let mut x = state.lock().unwrap();
        if let Some(it) = x.items.get_mut("e1") {
            it.location = "Monterrey".into();
        }
        x.bump("e1");
    }
    let mut e1 = store.get_event(e1.id).unwrap().unwrap();
    e1.summary = "Revisión (local)".into();
    handle.save_event(&e1).unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    {
        let x = state.lock().unwrap();
        assert_eq!(
            (x.items["e1"].subject.as_str(), x.items["e1"].location.as_str()),
            ("Revisión (local)", "Monterrey")
        );
    }
    let local = store.get_event(e1.id).unwrap().unwrap();
    assert_eq!((local.summary.as_str(), local.location.as_str()), ("Revisión (local)", "Monterrey"));
    assert!(
        drain(&mut events)
            .iter()
            .any(|e| matches!(e, PimEvent::Conflict { resolution: ConflictResolution::Merged, .. }))
    );

    // --- alta, respuesta a la invitación (la manda el servidor) y baja
    let start = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 11, 27, 16, 0, 0).unwrap();
    let id = handle
        .save_event(&Event {
            calendar: cal.id,
            summary: "Nueva".into(),
            start: EventTime::DateTime { utc: start, tzid: Some("America/Mexico_City".into()) },
            end: EventTime::DateTime {
                utc: start + chrono::Duration::hours(1),
                tzid: Some("America/Mexico_City".into()),
            },
            ..Default::default()
        })
        .unwrap();
    let reply = handle.rsvp(occ.id, PartStat::Accepted, Some("Ahí estaré")).unwrap();
    assert!(reply.is_none(), "Exchange manda la respuesta: sin correo iMIP");
    handle.sync_now_wait("ana-ex").await.unwrap();
    let created = store.get_event(id).unwrap().unwrap();
    assert_eq!(created.remote_id.as_deref(), Some("new1"));
    assert_eq!(created.uid, "uid-new1");
    {
        let x = state.lock().unwrap();
        let create = x.writes.iter().find(|w| w.contains("<t:Subject>Nueva</t:Subject>")).unwrap();
        assert!(create.contains(r#"<t:StartTimeZone Id="Central Standard Time (Mexico)"/>"#), "{create}");
        assert!(create.contains(r#"SendMeetingInvitations="SendToNone""#));
        assert!(
            create.contains(r#"<t:DistinguishedFolderId Id="F-cal"/>"#)
                || create.contains(r#"<t:FolderId Id="F-cal"/>"#)
        );
        let accept = x.writes.iter().find(|w| w.contains("<t:AcceptItem>")).unwrap();
        assert!(accept.contains(r#"<t:ReferenceItemId Id="o1"/>"#) && accept.contains("Ahí estaré"), "{accept}");
    }
    handle.delete_event(id).unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    assert!(!state.lock().unwrap().items.contains_key("new1"));

    // --- borrado en el servidor
    state.lock().unwrap().items.remove("e2");
    handle.sync_now_wait("ana-ex").await.unwrap();
    let names: Vec<String> = store.events_in_calendar(cal.id).unwrap().into_iter().map(|e| e.summary).collect();
    assert!(!names.contains(&"Comida".to_string()), "{names:?}");
    assert_eq!(store.ops_len().unwrap(), 0);
    handle.shutdown().await;
}

/// R4.5 con Exchange: «toda la serie» cambia el maestro (sin ChangeKey: el servidor cambia todas
/// las apariciones); «este y los siguientes» corta la regla del maestro el día anterior y crea otra
/// serie desde la aparición; deshacerlo devuelve la regla y borra la serie nueva.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ews_series_whole_and_split() {
    let state = Arc::new(Mutex::new(X::default()));
    {
        let mut x = state.lock().unwrap();
        x.add("m1", "Junta semanal", 16, "RecurringMaster", "uid-serie");
        x.add("o1", "Junta semanal", 23, "Occurrence", "uid-serie");
        x.add("o2", "Junta semanal", 30, "Occurrence", "uid-serie");
    }
    let h = Arc::clone(&state);
    let base = serve(move |req| handle(&h, req)).await;
    let url = format!("{base}/EWS/Exchange.asmx");
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("ana-ex", SecretPurpose::Incoming, "secreto").unwrap();
    let opts = PimSyncOptions {
        poll_interval: Duration::from_secs(3600),
        sync_on_start: false,
        user_tz: "America/Mexico_City".parse().unwrap(),
        graph_past_days: 3_650,
        graph_future_days: 3_650,
        ..Default::default()
    };
    let pim = PimAccount::from_account(&account(&url)).unwrap();
    let handle = PimSyncEngine::start(Arc::clone(&store), vec![pim], None, secrets, opts);
    handle.sync_now_wait("ana-ex").await.unwrap();
    let cal = store.calendars().unwrap().remove(0);
    let mut evs = store.events_in_calendar(cal.id).unwrap();
    evs.sort_by_key(|e| e.start.to_utc());
    assert_eq!(evs.len(), 2, "solo las apariciones, no el maestro");

    // --- toda la serie
    let mut edited = evs[0].clone();
    edited.summary = "Junta de equipo".into();
    iuredesk_pim_sync::ops::patch_graph_series(&store, &evs[0], &edited, "m1").unwrap().unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    {
        let x = state.lock().unwrap();
        let w = x.writes.iter().find(|w| w.contains(r#"<t:ItemId Id="m1"/>"#)).unwrap();
        assert!(
            w.contains(r#"ConflictResolution="AlwaysOverwrite""#)
                && w.contains("<t:Subject>Junta de equipo</t:Subject>"),
            "{w}"
        );
        assert!(x.items.values().all(|i| i.subject == "Junta de equipo"));
    }
    assert!(store.events_in_calendar(cal.id).unwrap().iter().all(|e| e.summary == "Junta de equipo"));

    // --- este y los siguientes (desde el 30)
    let evs = {
        let mut v = store.events_in_calendar(cal.id).unwrap();
        v.sort_by_key(|e| e.start.to_utc());
        v
    };
    let mut edited = evs[1].clone();
    edited.location = "Sala nueva".into();
    let (token, before) = iuredesk_pim_sync::ops::split_graph_series(&store, &evs[1], &edited, "m1").unwrap().unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    {
        let x = state.lock().unwrap();
        let cut = x.writes.iter().find(|w| w.contains("<t:EndDateRecurrence>")).unwrap();
        assert!(cut.contains("<t:EndDate>2026-11-29</t:EndDate>") && cut.contains(r#"<t:ItemId Id="m1"/>"#), "{cut}");
        let new = x.writes.iter().find(|w| w.contains("<m:CreateItem") && w.contains("<t:Recurrence>")).unwrap();
        assert!(new.contains("<t:Location>Sala nueva</t:Location>") && new.contains("<t:WeeklyRecurrence>"), "{new}");
        assert!(new.contains("<t:StartDate>2026-11-30</t:StartDate>"), "{new}");
    }

    // --- deshacer
    iuredesk_pim_sync::ops::undo_split_graph_series(&store, "m1", &token, &before).unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    {
        let x = state.lock().unwrap();
        let back = x.writes.iter().rev().find(|w| w.contains(r#"<t:ItemId Id="m1"/>"#)).unwrap();
        assert!(back.contains("<t:NumberedRecurrence>"), "regla original: {back}");
        assert!(!x.items.contains_key("new1"), "serie nueva borrada");
    }
    assert_eq!(store.ops_len().unwrap(), 0);
    handle.shutdown().await;
}

/// Hito 2: la carpeta de contactos como libreta (cambios incrementales con `SyncFolderItems`,
/// edición de un solo campo sin tocar el fax, alta y baja, estado olvidado por el servidor) y la
/// libreta global de la organización (`ResolveNames`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ews_contacts_and_directory() {
    let state = Arc::new(Mutex::new(X::default()));
    {
        let mut x = state.lock().unwrap();
        x.add_contact("c1", "Luis Gómez", "luis@contoso.com", "Ventas");
        x.add_contact("c2", "Marta Ruiz", "marta@contoso.com", "Compras");
    }
    let h = Arc::clone(&state);
    let base = serve(move |req| handle(&h, req)).await;
    let url = format!("{base}/EWS/Exchange.asmx");
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("ana-ex", SecretPurpose::Incoming, "secreto").unwrap();
    let opts = PimSyncOptions { poll_interval: Duration::from_secs(3600), sync_on_start: false, ..Default::default() };
    let pim = PimAccount::from_account(&account(&url)).unwrap();
    let handle = PimSyncEngine::start(Arc::clone(&store), vec![pim], None, secrets, opts);
    handle.sync_now_wait("ana-ex").await.unwrap();

    // --- primera pasada
    let book = store.address_books().unwrap().remove(0);
    assert_eq!(book.name, "Contactos");
    let by_name = |n: &str| store.contacts(book.id).unwrap().into_iter().find(|c| c.full_name == n);
    let luis = by_name("Luis Gómez").unwrap();
    assert_eq!((luis.title.as_str(), luis.primary_email()), ("Ventas", Some("luis@contoso.com")));
    assert!(luis.phones.is_empty(), "el fax no se lee");
    assert!(by_name("Marta Ruiz").is_some());

    // --- edición local: solo el puesto, con ChangeKey; el fax no se toca
    let mut l = luis.clone();
    l.title = "Director de ventas".into();
    handle.save_contact(&l, None).unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    {
        let x = state.lock().unwrap();
        let w = x.writes.iter().rev().find(|w| w.contains("<m:UpdateItem")).unwrap();
        assert!(w.contains(r#"FieldURI="contacts:JobTitle""#) && w.contains("ChangeKey=\"ck"), "{w}");
        assert!(!w.contains("PhoneNumber") && !w.contains("EmailAddress"), "{w}");
        assert_eq!(x.contacts["c1"].2, "Director de ventas");
    }

    // --- cambios del servidor: solo se pide lo cambiado
    {
        let mut x = state.lock().unwrap();
        x.seq += 1;
        let seq = x.seq;
        if let Some(c) = x.contacts.get_mut("c2") {
            c.2 = "Gerente de compras".into();
            c.3 = seq;
        }
        x.log.push(("Update", "c2".into()));
        x.contacts.remove("c1");
        x.log.push(("Delete", "c1".into()));
        x.fetched = 0;
    }
    handle.sync_now_wait("ana-ex").await.unwrap();
    assert_eq!(state.lock().unwrap().fetched, 1, "solo el contacto cambiado");
    assert_eq!(by_name("Marta Ruiz").unwrap().title, "Gerente de compras");
    assert!(by_name("Luis Gómez").is_none(), "borrado en el servidor");

    // --- alta y baja
    let id = handle
        .save_contact(
            &iuredesk_model::Contact {
                address_book: book.id,
                full_name: "Nora Paz".into(),
                emails: vec![iuredesk_model::Labeled::new("", "nora@contoso.com")],
                ..Default::default()
            },
            None,
        )
        .unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    assert_eq!(store.get_contact(id).unwrap().unwrap().remote_id.as_deref(), Some("cnew"));
    assert_eq!(state.lock().unwrap().contacts["cnew"].1, "nora@contoso.com");
    handle.delete_contact(id).unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    assert!(!state.lock().unwrap().contacts.contains_key("cnew"));

    // --- el servidor olvida el estado: todo de nuevo, sin perder nada
    {
        let mut x = state.lock().unwrap();
        x.forgot_before = x.log.len() + 1;
    }
    handle.sync_now_wait("ana-ex").await.unwrap();
    let names: Vec<String> = store.contacts(book.id).unwrap().into_iter().map(|c| c.full_name).collect();
    assert_eq!(names, ["Marta Ruiz"]);
    assert_eq!(store.ops_len().unwrap(), 0);

    // --- libreta global
    assert!(handle.has_directory());
    let found = handle.directory_search("ana", 10).await;
    assert_eq!(
        found.iter().map(|m| (m.name.as_deref().unwrap_or_default(), m.addr.as_str())).collect::<Vec<_>>(),
        [("Ana Pérez", "ana.perez@contoso.com"), ("Anabel Ríos", "anabel@contoso.com")]
    );
    handle.shutdown().await;
}

/// Hito 3: la carpeta de tareas como lista (incremental, edición del estado con `ChangeKey`, alta
/// y baja) y la disponibilidad de otras personas para el asistente de programación.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ews_tasks_and_availability() {
    let state = Arc::new(Mutex::new(X::default()));
    {
        let mut x = state.lock().unwrap();
        x.add_task("t1", "Enviar informe");
        x.add_task("t2", "Llamar a Bea");
        x.add("e1", "Revisión", 25, "Single", "uid-e1");
    }
    let h = Arc::clone(&state);
    let base = serve(move |req| handle(&h, req)).await;
    let url = format!("{base}/EWS/Exchange.asmx");
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let secrets = Arc::new(MemoryStore::default());
    secrets.set("ana-ex", SecretPurpose::Incoming, "secreto").unwrap();
    let opts = PimSyncOptions {
        poll_interval: Duration::from_secs(3600),
        sync_on_start: false,
        user_tz: "America/Mexico_City".parse().unwrap(),
        ..Default::default()
    };
    let pim = PimAccount::from_account(&account(&url)).unwrap();
    let handle = PimSyncEngine::start(Arc::clone(&store), vec![pim], None, secrets, opts);
    handle.sync_now_wait("ana-ex").await.unwrap();

    // --- primera pasada
    let list = store.task_lists().unwrap().remove(0);
    assert_eq!(list.name, "Tareas");
    let by = |s: &str| store.tasks(list.id).unwrap().into_iter().find(|t| t.summary == s);
    let t1 = by("Enviar informe").unwrap();
    assert_eq!(t1.priority, 1);
    assert_eq!(t1.due, Some(EventTime::Date(chrono::NaiveDate::from_ymd_opt(2026, 11, 20).unwrap())));
    assert!(by("Llamar a Bea").is_some());

    // --- completar: solo el estado, con ChangeKey
    let mut done = t1.clone();
    done.status = iuredesk_model::TaskStatus::Completed;
    handle.save_task(&done).unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    {
        let x = state.lock().unwrap();
        let w = x.writes.iter().rev().find(|w| w.contains("<m:UpdateItem")).unwrap();
        assert!(
            w.contains(r#"FieldURI="task:Status""#) && w.contains("ChangeKey=\"ck") && !w.contains("item:Subject"),
            "{w}"
        );
        assert_eq!(x.tasks["t1"].1, "Completed");
    }
    assert_eq!(by("Enviar informe").unwrap().status, iuredesk_model::TaskStatus::Completed);

    // --- alta y baja
    let id = handle
        .save_task(&iuredesk_model::Task { list: list.id, summary: "Revisar contrato".into(), ..Default::default() })
        .unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    assert_eq!(store.get_task(id).unwrap().unwrap().remote_id.as_deref(), Some("tnew"));
    handle.delete_task(id).unwrap();
    handle.sync_now_wait("ana-ex").await.unwrap();
    assert!(!state.lock().unwrap().tasks.contains_key("tnew"));
    // Borrado en el servidor.
    {
        let mut x = state.lock().unwrap();
        x.tasks.remove("t2");
        x.tlog.push(("Delete", "t2".into()));
    }
    handle.sync_now_wait("ana-ex").await.unwrap();
    assert!(by("Llamar a Bea").is_none());
    assert_eq!(store.ops_len().unwrap(), 0);

    // --- disponibilidad
    let cal = store.calendars().unwrap().remove(0);
    let from = chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 11, 25, 0, 0, 0).unwrap();
    let fb = handle
        .free_busy(
            cal.id,
            vec!["Bea@Contoso.com".into(), "nadie@otra.org".into()],
            from,
            from + chrono::Duration::days(1),
        )
        .await
        .unwrap();
    assert_eq!(fb.len(), 2);
    assert_eq!(fb[0].email, "bea@contoso.com");
    let busy = fb[0].busy.as_ref().unwrap();
    assert_eq!(busy.len(), 2);
    assert_eq!(busy[0].start.to_rfc3339(), "2026-11-25T15:00:00+00:00");
    assert!(!busy[0].tentative && busy[1].tentative);
    assert_eq!(fb[1].busy, None, "no se pudo consultar");
    handle.shutdown().await;
}
