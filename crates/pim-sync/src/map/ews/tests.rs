use super::*;
use crate::map::graph::{event_from_graph, event_patch, recurrence_to_rrule, rrule_to_recurrence};
use iuredesk_model::{AttendeeRole, EventStatus, EventTime, PartStat, Transparency};

const ITEM: &str = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
  <t:ItemId Id="AAMkOcc1" ChangeKey="DwAAAB1"/>
  <t:Subject>Revisión &amp; cierre</t:Subject>
  <t:Body BodyType="Text">Traer el informe.
Gracias</t:Body>
  <t:ReminderIsSet>true</t:ReminderIsSet>
  <t:ReminderMinutesBeforeStart>30</t:ReminderMinutesBeforeStart>
  <t:LastModifiedTime>2026-11-10T08:00:00Z</t:LastModifiedTime>
  <t:UID>040000008200E00074C5B7101A82E0080000000001</t:UID>
  <t:Start>2026-11-18T16:00:00Z</t:Start>
  <t:End>2026-11-18T17:00:00Z</t:End>
  <t:IsAllDayEvent>false</t:IsAllDayEvent>
  <t:LegacyFreeBusyStatus>Tentative</t:LegacyFreeBusyStatus>
  <t:Location>Sala 2</t:Location>
  <t:IsMeeting>true</t:IsMeeting>
  <t:IsCancelled>false</t:IsCancelled>
  <t:IsResponseRequested>true</t:IsResponseRequested>
  <t:CalendarItemType>Occurrence</t:CalendarItemType>
  <t:MyResponseType>Tentative</t:MyResponseType>
  <t:Organizer><t:Mailbox><t:Name>Bea Ruiz</t:Name><t:EmailAddress>bea@contoso.com</t:EmailAddress></t:Mailbox></t:Organizer>
  <t:RequiredAttendees>
    <t:Attendee><t:Mailbox><t:Name>Ana</t:Name><t:EmailAddress>Ana@Contoso.com</t:EmailAddress></t:Mailbox><t:ResponseType>Unknown</t:ResponseType></t:Attendee>
    <t:Attendee><t:Mailbox><t:Name>Bea Ruiz</t:Name><t:EmailAddress>bea@contoso.com</t:EmailAddress></t:Mailbox><t:ResponseType>Organizer</t:ResponseType></t:Attendee>
  </t:RequiredAttendees>
  <t:OptionalAttendees>
    <t:Attendee><t:Mailbox><t:Name>Ciro</t:Name><t:EmailAddress>ciro@contoso.com</t:EmailAddress></t:Mailbox><t:ResponseType>Decline</t:ResponseType></t:Attendee>
  </t:OptionalAttendees>
  <t:Resources>
    <t:Attendee><t:Mailbox><t:Name>Sala 2</t:Name><t:EmailAddress>sala2@contoso.com</t:EmailAddress></t:Mailbox><t:ResponseType>Accept</t:ResponseType></t:Attendee>
  </t:Resources>
  <t:StartTimeZone Id="Central Standard Time (Mexico)" Name="(UTC-06:00) Guadalajara, Ciudad de México, Monterrey"/>
</t:CalendarItem>"#;

fn parse(s: &str) -> Element {
    iuredesk_dav::dav::xml::parse(s.as_bytes()).unwrap()
}

#[test]
fn calendar_item_becomes_an_event() {
    let v = item_json(&parse(ITEM), &["ana@contoso.com".into()], chrono_tz::UTC).unwrap();
    assert_eq!(v["@odata.etag"], "DwAAAB1");
    assert_eq!(v["type"], "occurrence");
    let e = event_from_graph(&v);
    assert_eq!(e.remote_id.as_deref(), Some("AAMkOcc1"));
    assert_eq!(e.etag.as_deref(), Some("DwAAAB1"));
    assert_eq!(e.summary, "Revisión & cierre");
    assert_eq!(e.description, "Traer el informe.\nGracias");
    assert_eq!(e.location, "Sala 2");
    assert_eq!(e.uid, "040000008200E00074C5B7101A82E0080000000001");
    let EventTime::DateTime { utc, tzid } = &e.start else { panic!("hora") };
    assert_eq!(utc.to_rfc3339(), "2026-11-18T16:00:00+00:00");
    assert_eq!(tzid.as_deref(), Some("America/Mexico_City"));
    assert_eq!(e.status, EventStatus::Tentative);
    assert_eq!(e.transparency, Transparency::Opaque);
    assert_eq!(e.reminders.first().map(|r| r.minutes_before), Some(30));
    assert_eq!(e.organizer.as_ref().map(|o| o.addr.as_str()), Some("bea@contoso.com"));
    assert_eq!(e.attendees.len(), 4);
    // Nuestra respuesta sale de MyResponseType, no del «Unknown» de la lista.
    let me = e.attendees.iter().find(|a| a.mailbox.addr == "ana@contoso.com").unwrap();
    assert_eq!(me.partstat, PartStat::Tentative);
    let ciro = e.attendees.iter().find(|a| a.mailbox.addr == "ciro@contoso.com").unwrap();
    assert_eq!((ciro.role.clone(), ciro.partstat.clone()), (AttendeeRole::Optional, PartStat::Declined));
    let room = e.attendees.iter().find(|a| a.mailbox.addr == "sala2@contoso.com").unwrap();
    assert_eq!(room.role, AttendeeRole::NonParticipant);
    assert!(e.rrule.is_none(), "una aparición no trae regla");
}

#[test]
fn all_day_uses_its_zone() {
    let x = r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <t:ItemId Id="d1" ChangeKey="c"/><t:Subject>Asueto</t:Subject>
      <t:Start>2026-11-20T06:00:00Z</t:Start><t:End>2026-11-21T06:00:00Z</t:End>
      <t:IsAllDayEvent>true</t:IsAllDayEvent><t:LegacyFreeBusyStatus>Free</t:LegacyFreeBusyStatus>
      <t:StartTimeZone Id="Central Standard Time (Mexico)"/></t:CalendarItem>"#;
    let e = event_from_graph(&item_json(&parse(x), &[], chrono_tz::UTC).unwrap());
    assert_eq!(e.start, EventTime::Date(NaiveDate::from_ymd_opt(2026, 11, 20).unwrap()));
    assert_eq!(e.end, EventTime::Date(NaiveDate::from_ymd_opt(2026, 11, 21).unwrap()));
    assert_eq!(e.transparency, Transparency::Transparent);

    // De vuelta: medianoche en la zona del usuario y su nombre de Windows.
    let tz: Tz = "America/Mexico_City".parse().unwrap();
    let xml = create_xml(&event_patch(None, &e), tz);
    assert!(xml.contains("<t:Start>2026-11-20T06:00:00Z</t:Start>"), "{xml}");
    assert!(xml.contains("<t:IsAllDayEvent>true</t:IsAllDayEvent>"));
    assert!(xml.contains(r#"<t:StartTimeZone Id="Central Standard Time (Mexico)"/>"#), "{xml}");
}

#[test]
fn recurrence_round_trips_through_rrule() {
    for (xml, rrule) in [
        (
            "<t:WeeklyRecurrence><t:Interval>2</t:Interval><t:DaysOfWeek>Monday Wednesday</t:DaysOfWeek><t:FirstDayOfWeek>Sunday</t:FirstDayOfWeek></t:WeeklyRecurrence><t:NumberedRecurrence><t:StartDate>2026-11-02-06:00</t:StartDate><t:NumberOfOccurrences>10</t:NumberOfOccurrences></t:NumberedRecurrence>",
            "FREQ=WEEKLY;BYDAY=MO,WE;INTERVAL=2;COUNT=10",
        ),
        (
            "<t:RelativeMonthlyRecurrence><t:Interval>1</t:Interval><t:DaysOfWeek>Friday</t:DaysOfWeek><t:DayOfWeekIndex>Last</t:DayOfWeekIndex></t:RelativeMonthlyRecurrence><t:EndDateRecurrence><t:StartDate>2026-11-27</t:StartDate><t:EndDate>2027-06-30</t:EndDate></t:EndDateRecurrence>",
            "FREQ=MONTHLY;BYDAY=-1FR;UNTIL=20270630",
        ),
        (
            "<t:AbsoluteYearlyRecurrence><t:DayOfMonth>18</t:DayOfMonth><t:Month>November</t:Month></t:AbsoluteYearlyRecurrence><t:NoEndRecurrence><t:StartDate>2026-11-18</t:StartDate></t:NoEndRecurrence>",
            "FREQ=YEARLY;BYMONTH=11;BYMONTHDAY=18",
        ),
        (
            "<t:DailyRecurrence><t:Interval>3</t:Interval></t:DailyRecurrence><t:NoEndRecurrence><t:StartDate>2026-11-18</t:StartDate></t:NoEndRecurrence>",
            "FREQ=DAILY;INTERVAL=3",
        ),
    ] {
        let r = parse(&format!(r#"<t:Recurrence xmlns:t="{T}">{xml}</t:Recurrence>"#));
        let j = recurrence_json(&r).unwrap();
        assert_eq!(recurrence_to_rrule(&j).as_deref(), Some(rrule), "{xml}");
        // RRULE → Graph → EWS → Graph conserva la regla.
        let start = EventTime::Date(date_part(j["range"]["startDate"].as_str().unwrap()).unwrap());
        let back = recurrence_xml(&rrule_to_recurrence(rrule, &start).unwrap()).unwrap();
        let again =
            recurrence_json(&parse(&back.replace("<t:Recurrence>", &format!(r#"<t:Recurrence xmlns:t="{T}">"#))))
                .unwrap();
        assert_eq!(recurrence_to_rrule(&again).as_deref(), Some(rrule), "{back}");
    }
    assert!(recurrence_json(&parse(&format!(r#"<t:Recurrence xmlns:t="{T}"><t:Raro/></t:Recurrence>"#))).is_none());
}

#[test]
fn patch_becomes_ordered_fields() {
    let v = item_json(&parse(ITEM), &["ana@contoso.com".into()], chrono_tz::UTC).unwrap();
    let old = event_from_graph(&v);
    let mut new = old.clone();
    new.summary = "Revisión <final>".into();
    new.reminders.clear();
    new.attendees.retain(|a| a.role != AttendeeRole::Optional);
    let u = update_xml(&event_patch(Some(&old), &new), chrono_tz::UTC);
    assert!(
        u.contains(
            r#"<t:FieldURI FieldURI="item:Subject"/><t:CalendarItem><t:Subject>Revisión &lt;final&gt;</t:Subject>"#
        ),
        "{u}"
    );
    assert!(u.contains(
        r#"<t:FieldURI FieldURI="item:ReminderIsSet"/><t:CalendarItem><t:ReminderIsSet>false</t:ReminderIsSet>"#
    ));
    assert!(
        u.contains(r#"<t:DeleteItemField><t:FieldURI FieldURI="calendar:OptionalAttendees"/></t:DeleteItemField>"#),
        "{u}"
    );
    assert!(u.contains("<t:EmailAddress>sala2@contoso.com</t:EmailAddress>"));
    assert!(!u.contains("calendar:Start"), "la hora no cambió");
    assert!(update_xml(&event_patch(Some(&old), &old), chrono_tz::UTC).is_empty());

    // Mover la hora: la zona va antes que la hora; al crear, todo en el orden del esquema.
    new.start =
        EventTime::DateTime { utc: "2026-11-18T18:00:00Z".parse().unwrap(), tzid: Some("America/Mexico_City".into()) };
    new.end =
        EventTime::DateTime { utc: "2026-11-18T19:00:00Z".parse().unwrap(), tzid: Some("America/Mexico_City".into()) };
    let u = update_xml(&event_patch(Some(&old), &new), chrono_tz::UTC);
    let zone = u.find("calendar:StartTimeZone").unwrap();
    assert!(zone < u.find("calendar:Start\"").unwrap());
    assert!(u.contains("<t:Start>2026-11-18T18:00:00Z</t:Start>"), "{u}");
    let c = create_xml(&event_patch(None, &new), chrono_tz::UTC);
    let pos = |s: &str| c.find(s).unwrap_or_else(|| panic!("{s} en {c}"));
    assert!(pos("<t:Subject>") < pos("<t:Body") && pos("<t:Body") < pos("<t:Start>"));
    assert!(pos("<t:Start>") < pos("<t:End>") && pos("<t:End>") < pos("<t:Location>"));
    assert!(
        pos("<t:Location>") < pos("<t:RequiredAttendees>") && pos("<t:RequiredAttendees>") < pos("<t:StartTimeZone")
    );
}

#[test]
fn windows_zone_names() {
    assert_eq!(windows_zone("America/Mexico_City".parse().unwrap()), "Central Standard Time (Mexico)");
    assert_eq!(windows_zone(chrono_tz::UTC), "UTC");
    // Madrid no está en la tabla (la de su zona es París): una zona con los mismos desfases.
    let madrid = windows_zone("Europe/Madrid".parse().unwrap());
    assert!(
        [
            "Romance Standard Time",
            "W. Europe Standard Time",
            "Central Europe Standard Time",
            "Central European Standard Time"
        ]
        .contains(&madrid),
        "{madrid}"
    );
    // Monterrey (sin horario de verano desde 2022): cualquier zona UTC−6 fija sirve.
    let mty = windows_zone("America/Monterrey".parse().unwrap());
    assert!(["Central Standard Time (Mexico)", "Central America Standard Time"].contains(&mty), "{mty}");
}

#[test]
fn hostile_items_never_panic() {
    for x in [
        r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"/>"#,
        r#"<t:CalendarItem xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><t:ItemId Id="x"/><t:Start>basura</t:Start><t:ReminderMinutesBeforeStart>-9</t:ReminderMinutesBeforeStart><t:Recurrence><t:WeeklyRecurrence/></t:Recurrence></t:CalendarItem>"#,
    ] {
        if let Some(v) = item_json(&parse(x), &[], chrono_tz::UTC) {
            let _ = event_from_graph(&v);
        }
    }
}

proptest::proptest! {
    /// Campos hostiles en un `t:CalendarItem`: nunca pánico, y lo que se lee vuelve a escribirse.
    #[test]
    fn arbitrary_fields_never_panic(
        subject in "\\PC{0,80}",
        start in "\\PC{0,30}",
        zone in "\\PC{0,40}",
        days in "\\PC{0,40}",
        count in "\\PC{0,8}",
        kind in "(Single|Occurrence|Exception|RecurringMaster|\\PC{0,10})",
    ) {
        let x = format!(
            r#"<t:CalendarItem xmlns:t="{T}"><t:ItemId Id="i" ChangeKey="c"/><t:Subject>{}</t:Subject><t:Start>{}</t:Start><t:End>{}</t:End><t:CalendarItemType>{}</t:CalendarItemType><t:Recurrence><t:WeeklyRecurrence><t:Interval>{}</t:Interval><t:DaysOfWeek>{}</t:DaysOfWeek></t:WeeklyRecurrence><t:NumberedRecurrence><t:StartDate>{}</t:StartDate><t:NumberOfOccurrences>{}</t:NumberOfOccurrences></t:NumberedRecurrence></t:Recurrence><t:StartTimeZone Id="{}"/></t:CalendarItem>"#,
            esc(&subject), esc(&start), esc(&start), esc(&kind), esc(&count), esc(&days), esc(&start), esc(&count), esc(&zone)
        );
        let Ok(el) = iuredesk_dav::dav::xml::parse(x.as_bytes()) else { return Ok(()) };
        if let Some(v) = item_json(&el, &[], chrono_tz::UTC) {
            let e = event_from_graph(&v);
            let _ = create_xml(&event_patch(None, &e), chrono_tz::UTC);
            if let Some(r) = v.get("recurrence") {
                let _ = recurrence_xml(r);
            }
        }
    }
}
