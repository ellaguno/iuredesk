//! UX-31: grabaciones de reuniones hechas desde una cita («● Grabar» en Mi día). IureDesk recuerda
//! dónde quedaron el audio, su página en IureTranscribe y la carpeta de notas con la
//! transcripción, el resumen y la minuta, para mostrarlas en el detalle de la cita sin tocar la
//! cita en el servidor (con invitados, cambiar la descripción les mandaría un aviso).
//!
//! Tabla aditiva `event_recordings`: `UID` de la cita → lista de [`EventRecording`]. No cambia la
//! versión de esquema.

use chrono::{DateTime, Utc};
use redb::{ReadableDatabase, ReadableTable};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::tables::EVENT_RECORDINGS;
use crate::{Store, codec};

/// Grabaciones que se guardan por cita como máximo (las más viejas se descartan).
pub const MAX_RECORDINGS_PER_EVENT: usize = 50;
/// Longitud máxima de la clave (`UID`) y de los textos guardados.
const MAX_KEY: usize = 1024;
const MAX_TEXT: usize = 4096;

/// Una grabación de una cita.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecording {
    /// Título de la reunión.
    pub title: String,
    /// Ruta del audio.
    pub audio: String,
    /// Enlace que la abre en IureTranscribe (`iuretranscribe://open?path=…`), si lo dio.
    pub open_link: Option<String>,
    /// Carpeta de notas con la transcripción, el resumen y la minuta (relativa, con `/`).
    pub notes_folder: Option<String>,
    /// Cuándo se grabó.
    pub created: DateTime<Utc>,
}

fn key_of(uid: &str) -> String {
    codec::clip(uid.trim(), MAX_KEY).to_string()
}

fn clip_rec(r: &EventRecording) -> EventRecording {
    EventRecording {
        title: codec::clip(&r.title, 500).to_string(),
        audio: codec::clip(&r.audio, MAX_TEXT).to_string(),
        open_link: r.open_link.as_deref().map(|l| codec::clip(l, MAX_TEXT).to_string()),
        notes_folder: r.notes_folder.as_deref().map(|f| codec::clip(f, MAX_TEXT).to_string()),
        created: r.created,
    }
}

impl Store {
    /// Grabaciones de una cita (por `UID`), de la más vieja a la más nueva.
    pub fn event_recordings(&self, uid: &str) -> Result<Vec<EventRecording>> {
        let key = key_of(uid);
        if key.is_empty() {
            return Ok(Vec::new());
        }
        let tx = self.db.begin_read()?;
        let t = tx.open_table(EVENT_RECORDINGS)?;
        match t.get(key.as_str())? {
            Some(g) => codec::decode(g.value()),
            None => Ok(Vec::new()),
        }
    }

    /// Añade una grabación a una cita (la del mismo audio se reemplaza).
    pub fn add_event_recording(&self, uid: &str, rec: &EventRecording) -> Result<()> {
        let key = key_of(uid);
        if key.is_empty() {
            return Err(crate::StoreError::Invalid("grabación sin UID de cita".into()));
        }
        let rec = clip_rec(rec);
        self.write(|tx, _| {
            let mut t = tx.open_table(EVENT_RECORDINGS)?;
            let mut list: Vec<EventRecording> = match t.get(key.as_str())? {
                Some(g) => codec::decode(g.value()).unwrap_or_default(),
                None => Vec::new(),
            };
            list.retain(|r| r.audio != rec.audio);
            list.push(rec);
            let extra = list.len().saturating_sub(MAX_RECORDINGS_PER_EVENT);
            list.drain(..extra);
            let bytes = codec::encode(&list)?;
            t.insert(key.as_str(), bytes.as_slice())?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn rec(audio: &str) -> EventRecording {
        EventRecording {
            title: "Junta".into(),
            audio: audio.into(),
            open_link: Some(format!("iuretranscribe://open?path={audio}")),
            notes_folder: Some("Reuniones/2026-11-27 Junta".into()),
            created: Utc.with_ymd_and_hms(2026, 11, 27, 10, 0, 0).single().unwrap(),
        }
    }

    #[test]
    fn recordings_round_trip_dedupe_cap_and_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = Store::open(dir.path()).unwrap();
            assert!(s.event_recordings("uid-1").unwrap().is_empty());
            s.add_event_recording("uid-1", &rec("/a.wav")).unwrap();
            s.add_event_recording(" uid-1 ", &rec("/a.wav")).unwrap();
            s.add_event_recording("uid-1", &rec("/b.wav")).unwrap();
            assert!(s.add_event_recording("  ", &rec("/c.wav")).is_err());
            let huge = EventRecording { title: "x".repeat(10_000), ..rec(&"/y".repeat(10_000)) };
            s.add_event_recording("uid-2", &huge).unwrap();
            assert!(s.event_recordings("uid-2").unwrap()[0].title.len() <= 500);
            for i in 0..60 {
                s.add_event_recording("uid-3", &rec(&format!("/r{i}.wav"))).unwrap();
            }
            let many = s.event_recordings("uid-3").unwrap();
            assert_eq!(many.len(), MAX_RECORDINGS_PER_EVENT);
            assert_eq!(many.last().unwrap().audio, "/r59.wav");
        }
        let s = Store::open(dir.path()).unwrap();
        let v = s.event_recordings("uid-1").unwrap();
        assert_eq!(v.iter().map(|r| r.audio.as_str()).collect::<Vec<_>>(), ["/a.wav", "/b.wav"]);
    }
}
