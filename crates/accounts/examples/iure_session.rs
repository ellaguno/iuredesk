//! Diagnóstico: ¿qué ve IureDesk de la sesión compartida de Iurefficient? (sin secretos)
//! `cargo run -p iuredesk-accounts --example iure_session`
fn main() {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => return eprintln!("{e}"),
    };
    let acc = iurefficient_connect::account::active();
    println!("cuenta activa: {:?}", acc.as_ref().map(|a| (&a.domain, &a.email, &a.app)));
    let s = rt.block_on(iuredesk_accounts::iurefficient::iurefficient_session_with(
        acc.and_then(|a| a.account().ok()),
        &iuredesk_accounts::iurefficient::KeyringVault,
    ));
    println!("estado: {:?}", s.status);
}
