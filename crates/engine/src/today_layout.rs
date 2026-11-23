//! UX-15: tarjetas de «Mi día» que el usuario ordena, cambia de columna, oculta o vuelve a poner
//! (lo pidió el dueño 2026-11-09: «reordenar los widgets y poner o quitar algunos, de forma
//! minimalista»). Se guarda en las preferencias; lo desconocido (una tarjeta de una versión más
//! nueva) se ignora y lo nuevo que no esté en ningún sitio se agrega al final de la derecha.
//!
//! UX-19 (dueño 2026-11-23): las tarjetas se arrastran a otro sitio y cada una lleva un color suave
//! que la distingue (por omisión, los tonos del calendario); el usuario lo cambia o lo quita.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::lang::pick;

/// Una tarjeta de «Mi día».
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodayCard {
    Agenda,
    Tasks,
    Mail,
    Iurefficient,
    Reminders,
    /// S1.6: planes de IureProject que se siguen (solo con IureProject instalado).
    Projects,
    /// De una versión más nueva: se descarta al normalizar.
    #[serde(other)]
    Unknown,
}

impl TodayCard {
    /// Todas, en el orden por omisión.
    pub const ALL: [TodayCard; 6] = [
        TodayCard::Agenda,
        TodayCard::Tasks,
        TodayCard::Mail,
        TodayCard::Iurefficient,
        TodayCard::Reminders,
        TodayCard::Projects,
    ];

    /// Nombre visible.
    pub fn label(self) -> &'static str {
        match self {
            TodayCard::Agenda => pick("Agenda", "Agenda"),
            TodayCard::Tasks => pick("Tasks", "Tareas"),
            TodayCard::Mail => pick("Mail", "Correo"),
            TodayCard::Iurefficient => "Iurefficient",
            TodayCard::Reminders => pick("Reminders", "Recordatorios"),
            TodayCard::Projects => pick("Projects", "Proyectos"),
            TodayCard::Unknown => "",
        }
    }

    /// UX-19: tono por omisión (el de su módulo; la agenda, el azul del calendario; Iurefficient,
    /// el de su calendario). La UI lo aclara hasta un pastel.
    pub fn default_tint(self) -> u32 {
        match self {
            TodayCard::Agenda => 0x3B82F6,
            TodayCard::Tasks => 0x10B981,
            TodayCard::Mail => 0xF59E0B,
            TodayCard::Iurefficient => 0x0F766E,
            TodayCard::Reminders => 0xEC4899,
            TodayCard::Projects => 0x8B5CF6,
            TodayCard::Unknown => 0x64748B,
        }
    }
}

/// UX-19: tonos que se ofrecen para las tarjetas (los del calendario y compañía).
pub const TINTS: [u32; 10] =
    [0x3B82F6, 0x0EA5E9, 0x0F766E, 0x10B981, 0x84CC16, 0xF59E0B, 0xF97316, 0xEF4444, 0xEC4899, 0x8B5CF6];

/// UX-19: color elegido para una tarjeta (sin entrada: el de por omisión).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardTint {
    /// Sin color (la superficie del tema).
    None,
    /// Un tono `0xRRGGBB`.
    Color(u32),
}

/// Columna de una tarjeta (con poco ancho, la izquierda va antes que la derecha).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Left,
    Right,
}

/// Cómo se reparten las tarjetas.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodayLayout {
    #[serde(default)]
    pub left: Vec<TodayCard>,
    #[serde(default)]
    pub right: Vec<TodayCard>,
    /// Quitadas por el usuario.
    #[serde(default)]
    pub hidden: Vec<TodayCard>,
    /// UX-19: colores cambiados (las demás, su tono por omisión).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tints: BTreeMap<TodayCard, CardTint>,
}

impl Default for TodayLayout {
    fn default() -> Self {
        TodayLayout {
            left: vec![TodayCard::Agenda, TodayCard::Mail],
            right: vec![TodayCard::Tasks, TodayCard::Projects, TodayCard::Iurefficient, TodayCard::Reminders],
            hidden: Vec::new(),
            tints: BTreeMap::new(),
        }
    }
}

impl TodayLayout {
    /// Sin repetidas ni desconocidas; las que faltan van al final de la derecha.
    pub fn normalized(&self) -> TodayLayout {
        let mut seen = std::collections::HashSet::new();
        let mut keep = |v: &[TodayCard]| -> Vec<TodayCard> {
            v.iter()
                .copied()
                .filter(|c| *c != TodayCard::Unknown && seen.insert(*c))
                .take(TodayCard::ALL.len())
                .collect()
        };
        let left = keep(&self.left);
        let mut right = keep(&self.right);
        let hidden = keep(&self.hidden);
        for c in TodayCard::ALL {
            if !left.contains(&c) && !right.contains(&c) && !hidden.contains(&c) {
                right.push(c);
            }
        }
        let mut tints = self.tints.clone();
        tints.remove(&TodayCard::Unknown);
        TodayLayout { left, right, hidden, tints }
    }

    /// UX-19: tono de una tarjeta (`None`: sin color).
    pub fn tint(&self, c: TodayCard) -> Option<u32> {
        match self.tints.get(&c) {
            Some(CardTint::None) => None,
            Some(CardTint::Color(rgb)) => Some(rgb & 0xFF_FFFF),
            None => Some(c.default_tint()),
        }
    }

    /// UX-19: cambia el color de una tarjeta (`None` vuelve al de por omisión).
    pub fn set_tint(&mut self, c: TodayCard, t: Option<CardTint>) {
        if c == TodayCard::Unknown {
            return;
        }
        match t {
            Some(t) => {
                self.tints.insert(c, t);
            }
            None => {
                self.tints.remove(&c);
            }
        }
    }

    /// UX-19: lleva una tarjeta visible a `col` en la posición `index` (contada sin ella; se
    /// recorta al final). Lo que hace soltarla al arrastrar.
    pub fn move_to(&mut self, c: TodayCard, col: Column, index: usize) {
        let Some((from, i)) = self.find(c) else { return };
        self.col_mut(from).remove(i);
        let v = self.col_mut(col);
        let at = index.min(v.len());
        v.insert(at, c);
    }

    /// Dónde está una tarjeta visible (columna e índice).
    fn find(&self, c: TodayCard) -> Option<(Column, usize)> {
        if let Some(i) = self.left.iter().position(|x| *x == c) {
            return Some((Column::Left, i));
        }
        self.right.iter().position(|x| *x == c).map(|i| (Column::Right, i))
    }

    fn col_mut(&mut self, col: Column) -> &mut Vec<TodayCard> {
        match col {
            Column::Left => &mut self.left,
            Column::Right => &mut self.right,
        }
    }

    /// ¿Se puede subir (`up`) o bajar dentro de su columna?
    pub fn can_move(&self, c: TodayCard, up: bool) -> bool {
        match self.find(c) {
            Some((col, i)) => {
                let len = match col {
                    Column::Left => self.left.len(),
                    Column::Right => self.right.len(),
                };
                if up { i > 0 } else { i + 1 < len }
            }
            None => false,
        }
    }

    /// Sube o baja una tarjeta dentro de su columna.
    pub fn move_card(&mut self, c: TodayCard, up: bool) {
        let Some((col, i)) = self.find(c) else { return };
        let v = self.col_mut(col);
        let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < v.len()) };
        if let Some(j) = j {
            v.swap(i, j);
        }
    }

    /// Pasa una tarjeta a la otra columna (al final).
    pub fn switch_column(&mut self, c: TodayCard) {
        let Some((col, i)) = self.find(c) else { return };
        self.col_mut(col).remove(i);
        let other = if col == Column::Left { Column::Right } else { Column::Left };
        self.col_mut(other).push(c);
    }

    /// Columna de una tarjeta visible.
    pub fn column(&self, c: TodayCard) -> Option<Column> {
        self.find(c).map(|(col, _)| col)
    }

    /// Quita una tarjeta.
    pub fn hide(&mut self, c: TodayCard) {
        if c == TodayCard::Unknown {
            return;
        }
        if let Some((col, i)) = self.find(c) {
            self.col_mut(col).remove(i);
        }
        if !self.hidden.contains(&c) {
            self.hidden.push(c);
        }
    }

    /// Vuelve a poner una tarjeta quitada (al final de la columna más corta).
    pub fn show(&mut self, c: TodayCard) {
        self.hidden.retain(|x| *x != c);
        if self.find(c).is_some() || c == TodayCard::Unknown {
            return;
        }
        if self.left.len() <= self.right.len() {
            self.left.push(c);
        } else {
            self.right.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn default_matches_the_original_screen() {
        let l = TodayLayout::default().normalized();
        assert_eq!(l.left, vec![TodayCard::Agenda, TodayCard::Mail]);
        // S1.6: «Proyectos» bajo las tareas (sin IureProject no se dibuja).
        assert_eq!(l.right, vec![TodayCard::Tasks, TodayCard::Projects, TodayCard::Iurefficient, TodayCard::Reminders]);
        assert!(l.hidden.is_empty());
    }

    #[test]
    fn move_switch_hide_and_show() {
        let mut l = TodayLayout::default();
        assert!(!l.can_move(TodayCard::Agenda, true));
        l.move_card(TodayCard::Mail, true);
        assert_eq!(l.left, vec![TodayCard::Mail, TodayCard::Agenda]);
        l.move_card(TodayCard::Agenda, false); // ya es la última: nada
        assert_eq!(l.left, vec![TodayCard::Mail, TodayCard::Agenda]);
        l.switch_column(TodayCard::Tasks);
        assert_eq!(l.left, vec![TodayCard::Mail, TodayCard::Agenda, TodayCard::Tasks]);
        assert_eq!(l.column(TodayCard::Tasks), Some(Column::Left));
        l.hide(TodayCard::Agenda);
        assert_eq!(l.hidden, vec![TodayCard::Agenda]);
        assert_eq!(l.column(TodayCard::Agenda), None);
        l.show(TodayCard::Agenda);
        assert!(l.hidden.is_empty());
        assert_eq!(l.normalized(), l);
    }

    #[test]
    fn unknown_and_repeated_cards_are_dropped_and_missing_ones_return() {
        let json = r#"{"left":["mail","mail","weather_radar"],"right":[],"hidden":["agenda"]}"#;
        let l: TodayLayout = serde_json::from_str(json).unwrap();
        let n = l.normalized();
        assert_eq!(n.left, vec![TodayCard::Mail]);
        assert_eq!(n.hidden, vec![TodayCard::Agenda]);
        assert_eq!(n.right, vec![TodayCard::Tasks, TodayCard::Iurefficient, TodayCard::Reminders, TodayCard::Projects]);
    }

    #[test]
    fn a_saved_layout_gets_the_new_projects_card_at_the_end() {
        // S1.6: quien ya acomodó sus tarjetas ve «Proyectos» al final de la derecha.
        let json = r#"{"left":["agenda","mail"],"right":["tasks","iurefficient","reminders"],"hidden":[]}"#;
        let n = serde_json::from_str::<TodayLayout>(json).unwrap().normalized();
        assert_eq!(n.right.last(), Some(&TodayCard::Projects));
        assert_eq!(serde_json::to_string(&TodayCard::Projects).unwrap(), "\"projects\"");
    }

    #[test]
    fn drag_to_another_place() {
        let mut l = TodayLayout::default();
        // Dentro de su columna: la agenda debajo del correo.
        l.move_to(TodayCard::Agenda, Column::Left, 1);
        assert_eq!(l.left, vec![TodayCard::Mail, TodayCard::Agenda]);
        // A la otra columna, en medio.
        l.move_to(TodayCard::Mail, Column::Right, 1);
        assert_eq!(l.left, vec![TodayCard::Agenda]);
        assert_eq!(
            l.right,
            vec![TodayCard::Tasks, TodayCard::Mail, TodayCard::Projects, TodayCard::Iurefficient, TodayCard::Reminders]
        );
        // Más allá del final: al final. Una quitada no se mueve.
        l.move_to(TodayCard::Tasks, Column::Left, 50);
        assert_eq!(l.left, vec![TodayCard::Agenda, TodayCard::Tasks]);
        l.hide(TodayCard::Reminders);
        l.move_to(TodayCard::Reminders, Column::Left, 0);
        assert_eq!(l.left, vec![TodayCard::Agenda, TodayCard::Tasks]);
        assert_eq!(l.normalized(), l);
    }

    #[test]
    fn tints_default_change_and_survive_a_round_trip() {
        let mut l = TodayLayout::default();
        assert_eq!(l.tint(TodayCard::Agenda), Some(0x3B82F6), "el azul del calendario");
        assert!(TodayCard::ALL.iter().all(|c| l.tint(*c).is_some()), "todas con color por omisión");
        l.set_tint(TodayCard::Mail, Some(CardTint::None));
        l.set_tint(TodayCard::Tasks, Some(CardTint::Color(0x1EF4444)));
        assert_eq!(l.tint(TodayCard::Mail), None);
        assert_eq!(l.tint(TodayCard::Tasks), Some(0xEF4444), "solo 24 bits");
        let json = serde_json::to_string(&l).unwrap();
        let back: TodayLayout = serde_json::from_str(&json).unwrap();
        assert_eq!(back.normalized(), l.normalized());
        l.set_tint(TodayCard::Mail, None);
        assert_eq!(l.tint(TodayCard::Mail), Some(TodayCard::Mail.default_tint()));
        // Una preferencia anterior (sin colores) y una con tarjeta desconocida se leen.
        let old = r#"{"left":["agenda"],"right":[],"hidden":[],"tints":{"radar":"none","mail":{"color":255}}}"#;
        let n = serde_json::from_str::<TodayLayout>(old).unwrap().normalized();
        assert_eq!(n.tint(TodayCard::Mail), Some(255));
        assert!(!n.tints.contains_key(&TodayCard::Unknown));
    }

    fn card() -> impl Strategy<Value = TodayCard> {
        prop::sample::select(vec![
            TodayCard::Projects,
            TodayCard::Agenda,
            TodayCard::Tasks,
            TodayCard::Mail,
            TodayCard::Iurefficient,
            TodayCard::Reminders,
            TodayCard::Unknown,
        ])
    }

    proptest! {
        /// Cualquier secuencia de operaciones deja cada tarjeta conocida exactamente una vez.
        #[test]
        fn every_card_appears_once(
            left in prop::collection::vec(card(), 0..12),
            right in prop::collection::vec(card(), 0..12),
            hidden in prop::collection::vec(card(), 0..12),
            ops in prop::collection::vec((0u8..7, card()), 0..40),
        ) {
            let mut l = TodayLayout { left, right, hidden, tints: BTreeMap::new() }.normalized();
            for (op, c) in ops {
                match op {
                    0 => l.move_card(c, true),
                    1 => l.move_card(c, false),
                    2 => l.switch_column(c),
                    3 => l.hide(c),
                    4 => l.move_to(c, Column::Left, 1),
                    5 => l.move_to(c, Column::Right, 99),
                    _ => l.show(c),
                }
            }
            let all: Vec<TodayCard> = l.left.iter().chain(&l.right).chain(&l.hidden).copied().collect();
            for c in TodayCard::ALL {
                prop_assert_eq!(all.iter().filter(|x| **x == c).count(), 1);
            }
            prop_assert!(!all.contains(&TodayCard::Unknown));
        }
    }
}
