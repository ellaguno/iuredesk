//! Recompila el crate cuando cambian los identificadores OAuth que se incrustan con `option_env!`
//! (sin esto, una caché de compilación sin las variables los dejaba fuera del instalador).
fn main() {
    for v in ["IUREDESK_GOOGLE_CLIENT_ID", "IUREDESK_GOOGLE_CLIENT_SECRET", "IUREDESK_MS_CLIENT_ID"] {
        println!("cargo:rerun-if-env-changed={v}");
    }
}
