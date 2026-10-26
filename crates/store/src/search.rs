//! Índice de texto completo de mensajes (tantivy) y lenguaje de consulta.
//!
//! El índice guarda solo lo que no cambia en la vida de un mensaje: asunto, remitentes,
//! destinatarios, cuerpo, nombres de adjuntos, fecha y si tiene adjuntos. Lo que cambia a menudo
//! (carpeta al mover, banderas al leer) se filtra contra redb después de buscar, para no tener que
//! reindexar el cuerpo cada vez que el usuario lee un correo.
//!
//! Sin compresión zstd de tantivy (`zstd-sys` es C): el almacén de documentos usa lz4 (`lz4_flex`).

use std::ops::Bound;
use std::path::Path;
use std::sync::Mutex;

use chrono::{NaiveDate, NaiveTime};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, PhraseQuery, Query, RangeQuery, RegexQuery, TermQuery};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer};
use tantivy::{Index, IndexReader, IndexWriter, Order, ReloadPolicy, TantivyDocument, Term};

use crate::error::{Result, StoreError};

/// Versión del esquema del índice; si cambia, el índice se reconstruye.
const SEARCH_SCHEMA_VERSION: u32 = 1;
const TOKENIZER: &str = "iure";
/// Memoria del escritor de tantivy (un hilo).
const WRITER_HEAP: usize = 40_000_000;
/// Texto máximo que se indexa por mensaje (el resto del cuerpo no se busca).
pub const MAX_INDEXED_TEXT: usize = 256 * 1024;
/// Número máximo de palabras de una consulta.
const MAX_QUERY_TERMS: usize = 32;

/// Documento a indexar.
#[derive(Clone, Debug, Default)]
pub struct IndexDoc {
    /// Mensaje.
    pub msg: u64,
    /// Cuenta.
    pub account: u64,
    /// Carpeta al indexar (informativo; el filtro `in:` usa redb).
    pub folder: u64,
    /// Asunto.
    pub subject: String,
    /// Remitentes (nombres y direcciones).
    pub from: String,
    /// Destinatarios To/Cc/Bcc.
    pub to: String,
    /// Texto del cuerpo.
    pub body: String,
    /// Nombres de adjuntos.
    pub attachments: String,
    /// Fecha en milisegundos.
    pub date_ms: i64,
    /// Tiene adjuntos.
    pub has_attachments: bool,
}

#[derive(Clone, Copy)]
struct Fields {
    msg: Field,
    account: Field,
    folder: Field,
    subject: Field,
    from: Field,
    to: Field,
    body: Field,
    attachments: Field,
    date: Field,
    has_att: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let num = NumericOptions::from(INDEXED | FAST);
    let fields = Fields {
        msg: b.add_u64_field("msg", num.clone()),
        account: b.add_u64_field("account", num.clone()),
        folder: b.add_u64_field("folder", num.clone()),
        subject: b.add_text_field("subject", text.clone()),
        from: b.add_text_field("from", text.clone()),
        to: b.add_text_field("to", text.clone()),
        body: b.add_text_field("body", text.clone()),
        attachments: b.add_text_field("attachments", text),
        date: b.add_i64_field("date", num.clone()),
        has_att: b.add_u64_field("has_att", num),
    };
    (b.build(), fields)
}

fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(48))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

/// Índice de búsqueda de mensajes.
pub(crate) struct SearchIndex {
    reader: IndexReader,
    writer: Mutex<IndexWriter<TantivyDocument>>,
    f: Fields,
}

impl SearchIndex {
    /// Abre el índice en `dir`. Devuelve `true` si se tuvo que crear de cero (nuevo, esquema
    /// distinto o dañado): entonces hay que reindexar.
    pub(crate) fn open(dir: &Path) -> Result<(Self, bool)> {
        let version_file = dir.join("iuredesk-schema");
        let current = std::fs::read_to_string(&version_file).ok().and_then(|s| s.trim().parse::<u32>().ok());
        let (schema, f) = schema();
        let mut rebuilt = current != Some(SEARCH_SCHEMA_VERSION);
        if rebuilt && dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        std::fs::create_dir_all(dir)?;
        let index = match open_index(dir, schema.clone()) {
            Ok(i) => i,
            Err(_) => {
                // Índice dañado o incompatible: se reconstruye (se puede regenerar desde redb).
                std::fs::remove_dir_all(dir)?;
                std::fs::create_dir_all(dir)?;
                rebuilt = true;
                open_index(dir, schema)?
            }
        };
        std::fs::write(&version_file, SEARCH_SCHEMA_VERSION.to_string())?;
        index.tokenizers().register(TOKENIZER, analyzer());
        let reader = index.reader_builder().reload_policy(ReloadPolicy::Manual).try_into()?;
        let writer = index.writer_with_num_threads(1, WRITER_HEAP)?;
        Ok((Self { reader, writer: Mutex::new(writer), f }, rebuilt))
    }

    fn writer(&self) -> std::sync::MutexGuard<'_, IndexWriter<TantivyDocument>> {
        self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Añade (o reemplaza) documentos y confirma.
    pub(crate) fn upsert(&self, docs: &[IndexDoc]) -> Result<()> {
        let w = self.writer();
        for d in docs {
            w.delete_term(Term::from_field_u64(self.f.msg, d.msg));
            let mut doc = TantivyDocument::default();
            doc.add_u64(self.f.msg, d.msg);
            doc.add_u64(self.f.account, d.account);
            doc.add_u64(self.f.folder, d.folder);
            doc.add_text(self.f.subject, crate::codec::clip(&d.subject, MAX_INDEXED_TEXT));
            doc.add_text(self.f.from, crate::codec::clip(&d.from, MAX_INDEXED_TEXT));
            doc.add_text(self.f.to, crate::codec::clip(&d.to, MAX_INDEXED_TEXT));
            doc.add_text(self.f.body, crate::codec::clip(&d.body, MAX_INDEXED_TEXT));
            doc.add_text(self.f.attachments, crate::codec::clip(&d.attachments, MAX_INDEXED_TEXT));
            doc.add_i64(self.f.date, d.date_ms);
            doc.add_u64(self.f.has_att, u64::from(d.has_attachments));
            w.add_document(doc)?;
        }
        drop(w);
        self.commit()
    }

    /// Quita documentos y confirma.
    pub(crate) fn delete(&self, msgs: &[u64]) -> Result<()> {
        if msgs.is_empty() {
            return Ok(());
        }
        let w = self.writer();
        for &m in msgs {
            w.delete_term(Term::from_field_u64(self.f.msg, m));
        }
        drop(w);
        self.commit()
    }

    /// Borra todo el índice (antes de reindexar).
    pub(crate) fn clear(&self) -> Result<()> {
        self.writer().delete_all_documents()?;
        self.commit()
    }

    fn commit(&self) -> Result<()> {
        self.writer().commit()?;
        self.reader.reload()?;
        Ok(())
    }

    /// Número de documentos indexados.
    pub(crate) fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    fn tokens(&self, text: &str) -> Vec<String> {
        let mut a = analyzer();
        let mut out = Vec::new();
        let mut stream = a.token_stream(text);
        while stream.advance() {
            if out.len() >= MAX_QUERY_TERMS {
                break;
            }
            out.push(stream.token().text.clone());
        }
        out
    }

    fn text_query(&self, field: Field, text: &str, prefix_last: bool) -> Option<Box<dyn Query>> {
        let toks = self.tokens(text);
        match toks.as_slice() {
            [] => None,
            [one] => {
                if prefix_last && one.chars().count() >= 2 && one.chars().all(char::is_alphanumeric) {
                    // Búsqueda mientras se escribe: la última palabra libre es prefijo (la
                    // expresión también acepta la palabra exacta).
                    match RegexQuery::from_pattern(&format!("{one}.*"), field) {
                        Ok(rq) => Some(Box::new(rq)),
                        Err(_) => {
                            Some(Box::new(TermQuery::new(Term::from_field_text(field, one), IndexRecordOption::Basic)))
                        }
                    }
                } else {
                    Some(Box::new(TermQuery::new(Term::from_field_text(field, one), IndexRecordOption::Basic)))
                }
            }
            many => {
                let terms: Vec<Term> = many.iter().map(|t| Term::from_field_text(field, t)).collect();
                Some(Box::new(PhraseQuery::new(terms)))
            }
        }
    }

    /// Ejecuta la parte de texto/fecha/adjuntos de una consulta. Devuelve ids de mensaje por fecha
    /// descendente (hasta `k`).
    pub(crate) fn run(&self, q: &ParsedQuery, k: usize) -> Result<Vec<u64>> {
        let mut must: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let all_text = [self.f.subject, self.f.from, self.f.to, self.f.body, self.f.attachments];
        let last_free = q.terms.iter().rposition(|t| t.field == TermField::Any && !t.phrase);
        for (i, t) in q.terms.iter().enumerate() {
            let prefix = Some(i) == last_free;
            let fields: &[Field] = match t.field {
                TermField::Any => &all_text,
                TermField::From => std::slice::from_ref(&self.f.from),
                TermField::To => std::slice::from_ref(&self.f.to),
                TermField::Subject => std::slice::from_ref(&self.f.subject),
                TermField::Attachment => std::slice::from_ref(&self.f.attachments),
            };
            // Cada palabra suelta de un texto sin comillas es una condición aparte.
            let pieces: Vec<String> = if t.phrase { vec![t.text.clone()] } else { self.tokens(&t.text) };
            let n = pieces.len();
            for (j, piece) in pieces.iter().enumerate() {
                let alts: Vec<Box<dyn Query>> =
                    fields.iter().filter_map(|&f| self.text_query(f, piece, prefix && j + 1 == n)).collect();
                if !alts.is_empty() {
                    must.push((Occur::Must, Box::new(BooleanQuery::union(alts))));
                }
            }
        }
        if q.has_attachment {
            must.push((
                Occur::Must,
                Box::new(TermQuery::new(Term::from_field_u64(self.f.has_att, 1), IndexRecordOption::Basic)),
            ));
        }
        if q.after.is_some() || q.before.is_some() {
            let lo =
                q.after.map_or(Bound::Unbounded, |d| Bound::Included(Term::from_field_i64(self.f.date, day_ms(d))));
            let hi =
                q.before.map_or(Bound::Unbounded, |d| Bound::Excluded(Term::from_field_i64(self.f.date, day_ms(d))));
            must.push((Occur::Must, Box::new(RangeQuery::new(lo, hi))));
        }
        if must.is_empty() {
            return Ok(Vec::new());
        }
        let query = BooleanQuery::new(must);
        let searcher = self.reader.searcher();
        let top = TopDocs::with_limit(k.max(1)).order_by_fast_field::<i64>("date", Order::Desc);
        let hits = searcher.search(&query, &top)?;
        let mut out = Vec::with_capacity(hits.len());
        let mut cols: Vec<Option<tantivy::columnar::Column<u64>>> = Vec::new();
        for (_, addr) in hits {
            let seg = addr.segment_ord as usize;
            if cols.len() <= seg {
                cols.resize_with(seg.saturating_add(1), || None);
            }
            let slot = cols.get_mut(seg).ok_or_else(|| StoreError::Search("segmento".into()))?;
            if slot.is_none() {
                *slot = Some(searcher.segment_reader(addr.segment_ord).fast_fields().u64("msg")?);
            }
            if let Some(Some(col)) = cols.get(seg)
                && let Some(id) = col.first(addr.doc_id)
            {
                out.push(id);
            }
        }
        Ok(out)
    }
}

fn open_index(dir: &Path, schema: Schema) -> Result<Index> {
    let mmap = tantivy::directory::MmapDirectory::open(dir)?;
    Ok(Index::open_or_create(mmap, schema)?)
}

fn day_ms(d: NaiveDate) -> i64 {
    d.and_time(NaiveTime::MIN).and_utc().timestamp_millis()
}

/// Campo al que se aplica un término de búsqueda.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermField {
    /// Cualquier campo de texto.
    Any,
    /// `from:` / `de:`.
    From,
    /// `to:` / `para:` (incluye Cc y Bcc).
    To,
    /// `subject:` / `asunto:`.
    Subject,
    /// `filename:` / `adjunto:` (nombre de adjunto).
    Attachment,
}

/// Término de texto de una consulta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryTerm {
    /// Campo.
    pub field: TermField,
    /// Texto tal cual lo escribió el usuario.
    pub text: String,
    /// Iba entre comillas (frase exacta).
    pub phrase: bool,
}

/// Consulta interpretada.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    /// Términos de texto.
    pub terms: Vec<QueryTerm>,
    /// `has:attachment` / `tiene:adjunto`.
    pub has_attachment: bool,
    /// `before:AAAA-MM-DD` / `antes:` (exclusivo, días UTC).
    pub before: Option<NaiveDate>,
    /// `after:AAAA-MM-DD` / `despues:` (inclusivo, días UTC).
    pub after: Option<NaiveDate>,
    /// `in:carpeta` / `en:` (función especial o nombre, sin distinguir mayúsculas).
    pub folders: Vec<String>,
    /// `is:unread` (`Some(true)`) / `is:read` (`Some(false)`).
    pub unread: Option<bool>,
    /// `is:flagged` / `is:starred` / `es:destacado`.
    pub flagged: Option<bool>,
}

impl ParsedQuery {
    /// `true` si hay algo que el índice de texto deba resolver.
    pub fn needs_index(&self) -> bool {
        !self.terms.is_empty() || self.has_attachment || self.before.is_some() || self.after.is_some()
    }

    /// `true` si la consulta está vacía.
    pub fn is_empty(&self) -> bool {
        !self.needs_index() && self.folders.is_empty() && self.unread.is_none() && self.flagged.is_none()
    }
}

impl TermField {
    fn tf(self, text: String, phrase: bool) -> QueryTerm {
        QueryTerm { field: self, text, phrase }
    }
}

/// Interpreta una consulta de búsqueda.
///
/// Operadores (inglés / español): `from:`/`de:`, `to:`/`para:`, `subject:`/`asunto:`,
/// `filename:`/`adjunto:NOMBRE`, `has:attachment`/`tiene:adjunto`, `before:`/`antes:` y
/// `after:`/`despues:`/`después:` con fecha `AAAA-MM-DD`, `in:`/`en:CARPETA`,
/// `is:unread|read|flagged`/`es:noleido|leido|destacado`. Texto entre comillas = frase exacta.
/// Lo que no se reconoce se busca como texto libre; nunca falla.
pub fn parse_query(input: &str) -> ParsedQuery {
    let mut q = ParsedQuery::default();
    let input = crate::codec::clip(input, 4_096);
    let mut chars = input.chars().peekable();
    let mut count = 0usize;
    while chars.peek().is_some() && count < MAX_QUERY_TERMS {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        // Lee una palabra, respetando comillas tras `op:` o al inicio.
        let mut word = String::new();
        let mut quoted = None::<String>;
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            if c == '"' {
                chars.next();
                let mut s = String::new();
                for c in chars.by_ref() {
                    if c == '"' {
                        break;
                    }
                    s.push(c);
                }
                quoted = Some(s);
                break;
            }
            word.push(c);
            chars.next();
        }
        count = count.saturating_add(1);
        let (op, val, phrase) = match (word.split_once(':'), quoted) {
            (Some((op, "")), Some(qv)) => (Some(op.to_lowercase()), qv, true),
            (None, Some(qv)) if word.is_empty() => (None, qv, true),
            (None, Some(qv)) => (None, format!("{word}{qv}"), true),
            (Some((op, rest)), None) if !rest.is_empty() && !op.is_empty() => {
                (Some(op.to_lowercase()), rest.to_string(), false)
            }
            (_, Some(qv)) => (None, format!("{word}{qv}"), true),
            (_, None) => (None, word.clone(), false),
        };
        if val.trim().is_empty() {
            continue;
        }
        let lv = val.to_lowercase();
        match op.as_deref() {
            Some("from" | "de") => q.terms.push(TermField::From.tf(val, phrase)),
            Some("to" | "para" | "cc") => q.terms.push(TermField::To.tf(val, phrase)),
            Some("subject" | "asunto") => q.terms.push(TermField::Subject.tf(val, phrase)),
            Some("filename" | "adjunto" | "archivo") if !matches!(lv.as_str(), "si" | "sí" | "yes") => {
                q.terms.push(TermField::Attachment.tf(val, phrase));
            }
            Some("adjunto" | "archivo") => q.has_attachment = true,
            Some("has" | "tiene") if matches!(lv.as_str(), "attachment" | "attachments" | "adjunto" | "adjuntos") => {
                q.has_attachment = true;
            }
            Some("before" | "antes") if NaiveDate::parse_from_str(&val, "%Y-%m-%d").is_ok() => {
                q.before = NaiveDate::parse_from_str(&val, "%Y-%m-%d").ok();
            }
            Some("after" | "despues" | "después" | "desde") if NaiveDate::parse_from_str(&val, "%Y-%m-%d").is_ok() => {
                q.after = NaiveDate::parse_from_str(&val, "%Y-%m-%d").ok();
            }
            Some("in" | "en" | "folder" | "carpeta") => q.folders.push(lv),
            Some("is" | "es") => match lv.as_str() {
                "unread" | "noleido" | "no-leido" | "noleído" | "no-leído" | "nuevo" => q.unread = Some(true),
                "read" | "leido" | "leído" => q.unread = Some(false),
                "flagged" | "starred" | "destacado" | "marcado" => q.flagged = Some(true),
                _ => q.terms.push(TermField::Any.tf(format!("{}:{val}", op.as_deref().unwrap_or("")), false)),
            },
            Some(o) => q.terms.push(TermField::Any.tf(format!("{o}:{val}"), phrase)),
            None => q.terms.push(TermField::Any.tf(val, phrase)),
        }
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_operators() {
        let q = parse_query(
            r#"from:ana subject:"plan anual" has:attachment before:2026-01-31 after:2025-12-01 in:Inbox is:unread informe"#,
        );
        assert_eq!(q.terms.len(), 3);
        assert_eq!(q.terms[0], QueryTerm { field: TermField::From, text: "ana".into(), phrase: false });
        assert_eq!(q.terms[1], QueryTerm { field: TermField::Subject, text: "plan anual".into(), phrase: true });
        assert_eq!(q.terms[2].field, TermField::Any);
        assert!(q.has_attachment);
        assert_eq!(q.before, NaiveDate::from_ymd_opt(2026, 1, 31));
        assert_eq!(q.after, NaiveDate::from_ymd_opt(2025, 12, 1));
        assert_eq!(q.folders, vec!["inbox".to_string()]);
        assert_eq!(q.unread, Some(true));
    }

    #[test]
    fn parses_spanish_aliases_and_garbage() {
        let q = parse_query("de:luis para:marta tiene:adjunto antes:2026-02-30 en:enviados es:destacado");
        assert_eq!(q.terms.len(), 3, "{q:?}"); // la fecha inválida queda como texto
        assert!(q.has_attachment);
        assert_eq!(q.flagged, Some(true));
        assert_eq!(q.folders, vec!["enviados".to_string()]);
        assert!(parse_query("").is_empty());
        assert!(parse_query("   \"  ").is_empty());
        let q = parse_query("\"sin cerrar");
        assert_eq!(q.terms[0].text, "sin cerrar");
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics(s in "\\PC{0,300}") {
            let q = parse_query(&s);
            proptest::prop_assert!(q.terms.len() <= MAX_QUERY_TERMS);
        }
    }

    #[test]
    fn index_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let (idx, rebuilt) = SearchIndex::open(dir.path()).unwrap();
        assert!(rebuilt);
        idx.upsert(&[
            IndexDoc {
                msg: 1,
                subject: "Presupuesto de la reunión".into(),
                from: "Ana Pérez ana@example.com".into(),
                body: "Adjunto el presupuesto revisado".into(),
                date_ms: 1_000,
                has_attachments: true,
                attachments: "presupuesto.xlsx".into(),
                ..Default::default()
            },
            IndexDoc {
                msg: 2,
                subject: "Cena".into(),
                from: "Luis luis@example.org".into(),
                date_ms: 2_000,
                ..Default::default()
            },
        ])
        .unwrap();
        assert_eq!(idx.num_docs(), 2);
        let run = |s: &str| idx.run(&parse_query(s), 10).unwrap();
        assert_eq!(run("reunion"), vec![1]); // sin acento encuentra con acento
        assert_eq!(run("presu"), vec![1]); // prefijo
        assert_eq!(run("from:ana@example.com"), vec![1]);
        assert_eq!(run("from:luis"), vec![2]);
        assert_eq!(run("has:attachment"), vec![1]);
        assert_eq!(run("example"), vec![2, 1]); // fecha descendente
        assert!(run("from:nadie").is_empty());
        idx.delete(&[1]).unwrap();
        assert_eq!(run("example"), vec![2]);
        drop(idx);
        let (idx2, rebuilt) = SearchIndex::open(dir.path()).unwrap();
        assert!(!rebuilt);
        assert_eq!(idx2.num_docs(), 1);
    }
}
