//! Descubre la configuración de un correo y, si se da `IUREDESK_TEST_PASSWORD`, prueba la conexión
//! con el mejor candidato (con contraseña; un candidato OAuth se prueba como contraseña de app).
//! No guarda nada en el llavero ni en disco.
//!
//! ```text
//! cargo run -p iuredesk-accounts --example probe -- ana@example.com
//! IUREDESK_TEST_PASSWORD=… cargo run -p iuredesk-accounts --example probe -- ana@example.com
//! ```

use iuredesk_accounts::autoconfig;
use iuredesk_accounts::probe::{test_incoming, test_outgoing};
use iuredesk_model::account::{AuthMethod, ServerConfig};

fn spanish() -> bool {
    std::env::var("LANG").map(|l| l.starts_with("es")).unwrap_or(false)
}

fn show(s: &ServerConfig) -> String {
    format!("{:?} {}:{} {:?} {:?} user={}", s.protocol, s.host, s.port, s.security, s.auth, s.username)
}

fn main() {
    let Some(email) = std::env::args().nth(1) else {
        eprintln!("uso: probe <correo>");
        std::process::exit(2);
    };
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("no se pudo crear el runtime: {e}");
            std::process::exit(1);
        }
    };
    let es = spanish();
    rt.block_on(async {
        let started = std::time::Instant::now();
        let d = match autoconfig::discover(&email).await {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        println!(
            "{} → {:?} ({} ms){}",
            d.email,
            d.kind,
            started.elapsed().as_millis(),
            if d.timed_out { " [timeout parcial]" } else { "" }
        );
        for (i, c) in d.candidates.iter().enumerate() {
            println!(
                "  #{i} [{:?} {:?} {}] {}",
                c.source,
                c.confidence(),
                c.score,
                c.provider.as_deref().unwrap_or("-")
            );
            println!("     in:  {}", show(&c.incoming));
            if let Some(o) = &c.outgoing {
                println!("     out: {}", show(o));
            }
        }
        if let Some(h) = &d.caldav {
            println!("  CalDAV:  {} ({:?})", h.url, h.source);
        }
        if let Some(h) = &d.carddav {
            println!("  CardDAV: {} ({:?})", h.url, h.source);
        }
        for n in &d.notes {
            println!("  · {}", n.text(es));
        }

        let Ok(password) = std::env::var("IUREDESK_TEST_PASSWORD") else { return };
        let Some(best) = d.best() else { return };
        let as_password = |s: &ServerConfig| {
            let mut s = s.clone();
            if matches!(s.auth, AuthMethod::OAuth2 { .. }) {
                s.auth = AuthMethod::AppPassword;
            }
            s
        };
        match test_incoming(&as_password(&best.incoming), &password).await {
            Ok(r) => {
                println!("entrante OK: {} carpetas {:?}, mensajes {:?}", r.folder_count, r.folders, r.message_count)
            }
            Err(e) => println!("entrante: {} ({e})", e.message(es)),
        }
        if let Some(o) = &best.outgoing {
            match test_outgoing(&as_password(o), &password).await {
                Ok(_) => println!("saliente OK"),
                Err(e) => println!("saliente: {} ({e})", e.message(es)),
            }
        }
    });
}
