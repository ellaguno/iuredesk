//! UX-15: tarjetas de «Mi día» que el usuario ordena, cambia de columna, oculta o vuelve a poner
//! (lo pidió el dueño 2026-11-09: «reordenar los widgets y poner o quitar algunos, de forma
//! minimalista»). Se guarda en las preferencias; lo desconocido (una tarjeta de una versión más
//! nueva) se ignora y lo nuevo que no esté en ningún sitio se agrega al final de la derecha.

use serde::{Deserialize, Serialize};

use crate::lang::pick;

/// Una tarjeta de «Mi día».
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodayCard {
    Agenda,
    Tasks,
    Mail,
    Iurefficient,
    Reminders,
    /// De una versión más nueva: se descarta al normalizar.
    #[serde(other)]
    Unknown,
}

impl TodayCard {
    /// Todas, en el orden por omisión.
    pub const ALL: [TodayCard; 5] =
        [TodayCard::Agenda, TodayCard::Tasks, TodayCard::Mail, TodayCard::Iurefficient, TodayCard::Reminders];

    /// Nombre visible.
    pub fn label(self) -> &'static str {
        match self {
            TodayCard::Agenda => pick("Agenda", "Agenda"),
            TodayCard::Tasks => pick("Tasks", "Tareas"),
            TodayCard::Mail => pick("Mail", "Correo"),
            TodayCard::Iurefficient => "Iurefficient",
            TodayCard::Reminders => pick("Reminders", "Recordatorios"),
            TodayCard::Unknown => "",
        }
    }
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
}

impl Default for TodayLayout {
    fn default() -> Self {
        TodayLayout {
            left: vec![TodayCard::Agenda, TodayCard::Mail],
            right: vec![TodayCard::Tasks, TodayCard::Iurefficient, TodayCard::Reminders],
            hidden: Vec::new(),
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
        TodayLayout { left, right, hidden }
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
        assert_eq!(l.right, vec![TodayCard::Tasks, TodayCard::Iurefficient, TodayCard::Reminders]);
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
        assert_eq!(n.right, vec![TodayCard::Tasks, TodayCard::Iurefficient, TodayCard::Reminders]);
    }

    fn card() -> impl Strategy<Value = TodayCard> {
        prop::sample::select(vec![
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
            ops in prop::collection::vec((0u8..5, card()), 0..40),
        ) {
            let mut l = TodayLayout { left, right, hidden }.normalized();
            for (op, c) in ops {
                match op {
                    0 => l.move_card(c, true),
                    1 => l.move_card(c, false),
                    2 => l.switch_column(c),
                    3 => l.hide(c),
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
