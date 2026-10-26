//! Descubre calendarios y libretas de un servidor CalDAV/CardDAV.
//!
//! ```sh
//! IUREDESK_DAV_PASSWORD=… cargo run -p iuredesk-dav --example dav_probe -- <url> <usuario>
//! ```
//!
//! Con `IUREDESK_DAV_BEARER=1`, la «contraseña» se usa como token OAuth (Google).

use iuredesk_dav::dav::{Collection, Service};
use iuredesk_dav::{BasicAuth, BearerAuth, DavClient, DavOptions, Error};

fn print(kind: &str, result: Result<(iuredesk_dav::Discovery, Vec<Collection>), Error>) {
    match result {
        Ok((d, cols)) => {
            println!("{kind}: proveedor {:?}", d.provider);
            if let Some(p) = &d.principal {
                println!("  principal: {p}");
            }
            if let Some(n) = &d.display_name {
                println!("  usuario:   {n}");
            }
            if let Some(o) = &d.schedule_outbox {
                println!("  buzón de salida (free/busy): {o}");
            }
            for c in cols {
                println!(
                    "  • {:<32} {}{}{}",
                    c.display_name.as_deref().unwrap_or("(sin nombre)"),
                    c.url,
                    c.color.as_deref().map(|x| format!("  color {x}")).unwrap_or_default(),
                    if c.read_only { "  [solo lectura]" } else { "" }
                );
                if !c.components.is_empty() {
                    println!("      componentes: {}", c.components.join(", "));
                }
                println!(
                    "      sync-token: {}  ctag: {}",
                    c.sync_token.as_deref().unwrap_or("—"),
                    c.ctag.as_deref().unwrap_or("—")
                );
            }
        }
        Err(e) => println!("{kind}: {e}"),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(url), Some(user)) = (args.next(), args.next()) else {
        eprintln!("uso: dav_probe <url> <usuario>   (contraseña en IUREDESK_DAV_PASSWORD)");
        std::process::exit(2);
    };
    let Ok(password) = std::env::var("IUREDESK_DAV_PASSWORD") else {
        eprintln!("falta la variable IUREDESK_DAV_PASSWORD");
        std::process::exit(2);
    };
    let client = if std::env::var("IUREDESK_DAV_BEARER").is_ok() {
        DavClient::new(BearerAuth::new(&password), DavOptions::default())
    } else {
        DavClient::new(BasicAuth::new(&user, &password), DavOptions::default())
    };
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    print("CalDAV", client.discover_collections(&url, Service::CalDav).await);
    print("CardDAV", client.discover_collections(&url, Service::CardDav).await);
}
