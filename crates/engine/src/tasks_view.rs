//! S1.34: cómo se ven las tareas: una lista, un tablero por estado o agrupadas por caso.

use serde::{Deserialize, Serialize};

use crate::lang::pick;

/// Vista de las tareas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskView {
    /// Una lista a la vez (la de siempre).
    #[default]
    List,
    /// Columnas por estado: por hacer, en curso, terminadas (las canceladas, plegadas).
    Board,
    /// Agrupadas por su caso de Iurefficient (y el plan de IureProject de ese caso).
    ByCase,
}

impl TaskView {
    pub const ALL: [TaskView; 3] = [TaskView::List, TaskView::Board, TaskView::ByCase];

    /// Id estable (comandos `tasks.view.<key>`).
    pub fn key(self) -> &'static str {
        match self {
            TaskView::List => "list",
            TaskView::Board => "board",
            TaskView::ByCase => "case",
        }
    }

    pub fn from_key(s: &str) -> Option<TaskView> {
        TaskView::ALL.into_iter().find(|v| v.key() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            TaskView::List => pick("List", "Lista"),
            TaskView::Board => pick("Board", "Tablero"),
            TaskView::ByCase => pick("By case", "Por caso"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip() {
        for v in TaskView::ALL {
            assert_eq!(TaskView::from_key(v.key()), Some(v));
        }
        assert_eq!(TaskView::from_key("x"), None);
        assert_eq!(TaskView::default(), TaskView::List);
    }
}
