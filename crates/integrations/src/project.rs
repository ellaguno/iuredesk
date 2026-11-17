//! S1.6: estado de un proyecto de IureProject para «Hoy», sin abrir su ventana.
//!
//! IureProject expone por MCP (`iureproject --mcp`) la herramienta `project_overview`: avance,
//! tareas atrasadas e hitos próximos a la fecha de estado del plan. Aquí se abre cada archivo con
//! `file_open` y se lee ese resumen; el resultado es hostil (se acota todo). Para trabajar en el
//! plan, la UI abre IureProject con [`Suite::open_in_iureproject`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::error::Result;
use crate::suite::{Outcome, SisterApp, Suite, absolute};

/// Proyectos que se siguen como mucho.
pub const MAX_PROJECTS: usize = 12;
/// Tareas y hitos por proyecto como mucho.
const MAX_LINES: usize = 10;
/// Caracteres de un nombre como mucho.
const MAX_NAME: usize = 120;
/// Tiempo máximo de cada llamada.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Una tarea o un hito del resumen.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ProjectTask {
    pub uid: u32,
    pub name: String,
    /// Fin (`AAAA-MM-DDTHH:MM:SS`), si tiene.
    pub finish: Option<String>,
    pub percent_complete: u8,
    pub critical: bool,
}

/// Resumen de un proyecto (lo que `project_overview` devuelve, acotado).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ProjectOverview {
    pub name: String,
    /// Fecha a la que se mide (`AAAA-MM-DD`).
    pub as_of: String,
    pub finish: Option<String>,
    pub percent_complete: f64,
    pub tasks: usize,
    pub done: usize,
    pub late: usize,
    pub late_tasks: Vec<ProjectTask>,
    pub upcoming_milestones: Vec<ProjectTask>,
}

impl ProjectOverview {
    /// Recorta lo que llegó: nombres, listas y números fuera de rango.
    fn bounded(mut self) -> Self {
        let cut = |s: &mut String| {
            if s.chars().count() > MAX_NAME {
                *s = s.chars().take(MAX_NAME).collect::<String>() + "…";
            }
        };
        cut(&mut self.name);
        self.as_of = self.as_of.chars().take(10).collect();
        self.finish = self.finish.map(|f| f.chars().take(19).collect());
        self.percent_complete =
            if self.percent_complete.is_finite() { self.percent_complete.clamp(0.0, 100.0) } else { 0.0 };
        for list in [&mut self.late_tasks, &mut self.upcoming_milestones] {
            list.truncate(MAX_LINES);
            for t in list.iter_mut() {
                cut(&mut t.name);
                t.finish = t.finish.take().map(|f| f.chars().take(19).collect());
                t.percent_complete = t.percent_complete.min(100);
            }
        }
        self
    }
}

/// Un proyecto seguido: el archivo y su resumen (o por qué no se pudo leer).
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectStatus {
    pub file: PathBuf,
    pub overview: std::result::Result<ProjectOverview, crate::suite::Text>,
}

impl Suite {
    /// S1.6: el resumen de cada archivo de proyecto (como mucho [`MAX_PROJECTS`]), con un solo
    /// proceso `iureproject --mcp` que los abre uno tras otro. Un archivo que falta o no se puede
    /// leer da su error en lenguaje humano y no detiene a los demás.
    pub fn project_overviews(&self, files: &[PathBuf]) -> Result<Outcome<Vec<ProjectStatus>>> {
        let mut client = match self.connect_mcp(SisterApp::Project)? {
            Outcome::Ready(c) => c,
            Outcome::NotInstalled(h) => return Ok(Outcome::NotInstalled(h)),
            Outcome::NotAvailable(t) => return Ok(Outcome::NotAvailable(t)),
        };
        let mut out = Vec::new();
        for file in files.iter().take(MAX_PROJECTS) {
            let overview = overview_of(&mut client, file);
            out.push(ProjectStatus { file: file.clone(), overview });
        }
        Ok(Outcome::Ready(out))
    }
}

fn overview_of(client: &mut crate::McpClient, file: &Path) -> std::result::Result<ProjectOverview, crate::suite::Text> {
    use crate::suite::Text;
    let path = absolute(file).map_err(|_| {
        Text::new(
            format!("The file {} isn't there anymore.", file.display()),
            format!("El archivo {} ya no está.", file.display()),
        )
    })?;
    let unreadable = |e: String| {
        Text::new(
            format!("IureProject couldn't read this plan: {e}"),
            format!("IureProject no pudo leer este plan: {e}"),
        )
    };
    client
        .call_tool("file_open", json!({ "path": path.to_string_lossy() }), Some(TIMEOUT))
        .and_then(|r| r.into_ok())
        .map_err(|e| unreadable(e.to_string()))?;
    let data = client
        .call_tool("project_overview", json!({}), Some(TIMEOUT))
        .and_then(|r| r.into_ok())
        .map_err(|e| unreadable(e.to_string()))?
        .data()
        .ok_or_else(|| unreadable("sin resumen".into()))?;
    serde_json::from_value::<ProjectOverview>(data).map(ProjectOverview::bounded).map_err(|e| unreadable(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_parses_and_is_bounded() {
        let v = json!({
            "name": "x".repeat(500),
            "as_of": "2026-11-17",
            "finish": "2027-01-31T17:00:00",
            "percent_complete": 250.0,
            "tasks": 40, "done": 10, "late": 2, "ignored_field": true,
            "late_tasks": (0..30).map(|i| json!({"uid": i, "name": format!("T{i}"), "finish": "2026-11-10T17:00:00", "percent_complete": 120, "critical": true})).collect::<Vec<_>>(),
            "upcoming_milestones": [{"uid": 9, "name": "Entrega", "finish": "2026-11-30T17:00:00"}]
        });
        let o = serde_json::from_value::<ProjectOverview>(v).unwrap().bounded();
        assert_eq!(o.name.chars().count(), MAX_NAME + 1);
        assert_eq!(o.percent_complete, 100.0);
        assert_eq!(o.late_tasks.len(), MAX_LINES);
        assert_eq!(o.late_tasks[0].percent_complete, 100);
        assert_eq!(o.upcoming_milestones[0].name, "Entrega");
        // Lo que falta toma su valor de omisión.
        let empty = serde_json::from_value::<ProjectOverview>(json!({})).unwrap();
        assert_eq!(empty, ProjectOverview::default());
    }
}
