//! Inicio de sesión real con Google o Microsoft (abre el navegador del sistema).
//!
//! ```sh
//! IUREDESK_GOOGLE_CLIENT_ID=… IUREDESK_GOOGLE_CLIENT_SECRET=… \
//!     cargo run -p iuredesk-oauth --example login -- google
//! IUREDESK_MS_CLIENT_ID=… cargo run -p iuredesk-oauth --example login -- microsoft [correo]
//! ```
//!
//! Muestra el correo, los alcances concedidos y la caducidad; prueba un refresco para la API y
//! construye la respuesta XOAUTH2. Nunca imprime los tokens ni guarda nada en el llavero.

use std::process::ExitCode;

use iuredesk_oauth::{
    CancellationToken, DEFAULT_LOGIN_TIMEOUT, OAuthSettings, TokenAudience, http_client, sasl, start_login, token,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}\n{}", e.message(true));
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), iuredesk_oauth::Error> {
    let mut args = std::env::args().skip(1);
    let which = args.next().unwrap_or_default();
    let hint = args.next();
    let settings = OAuthSettings::default();
    let provider = match which.as_str() {
        "google" => settings.google()?,
        "microsoft" => settings.microsoft()?,
        _ => {
            eprintln!("uso: login google|microsoft [correo]");
            return Ok(());
        }
    };

    let pending = start_login(&provider, hint.as_deref()).await?;
    println!("Esperando en {} …", pending.redirect_uri());
    if let Err(e) = pending.open_browser() {
        eprintln!("{}", e.message(true));
    }
    println!("Si el navegador no se abrió, abre este enlace:\n\n{}\n", pending.authorize_url());

    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            c2.cancel();
        }
    });
    let http = http_client()?;
    let tokens = pending.finish(&http, DEFAULT_LOGIN_TIMEOUT, &cancel).await?;
    let email = tokens.id_claims.as_ref().and_then(|c| c.best_email()).unwrap_or("?").to_string();
    println!("Sesión iniciada: {email}");
    println!("Caduca: {}", tokens.expires_at);
    println!("Alcances: {}", tokens.scopes.join(" "));
    println!("Token de refresco: {}", if tokens.refresh_token.is_some() { "sí" } else { "NO" });
    println!("XOAUTH2 listo ({} caracteres)", sasl::xoauth2(&email, &tokens.access_token)?.len());

    if let Some(rt) = tokens.refresh_token.as_deref() {
        let api = token::refresh(&http, &provider, rt, TokenAudience::Api).await?;
        println!("Refresco para la API: correcto, caduca {}", api.expires_at);
    }
    Ok(())
}
