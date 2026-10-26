//! Códec IMAP propio: delimitación en [`crate::transport`], valores y respuestas aquí.

mod parse;
mod types;

pub use parse::{
    Cursor, MAX_DEPTH, ParseError, body_structure, parse_code, parse_internal_date, parse_items, parse_response,
};
pub use types::*;
