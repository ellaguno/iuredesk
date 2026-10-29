//! R1.2: con IureDesk abierto (aquí, solo su puente IPC), un segundo `iuredesk archivo.eml
//! mailto:…` le pasa sus argumentos —con rutas absolutas— y termina sin abrir otra ventana.

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn second_instance_forwards_args_and_exits() {
    let data = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let store = Arc::new(iuredesk_store::Store::open(data.path().join("store")).unwrap());
    let got: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let sink = got.clone();
    let bridge = iuredesk_mcp::start_bridge(
        store,
        iuredesk_mcp::BridgeOptions {
            runtime_dir: Some(runtime.path().join("iuredesk")),
            on_open: Some(Arc::new(move |a: Vec<String>| sink.lock().unwrap().push(a))),
            ..Default::default()
        },
    )
    .unwrap();

    let run = |args: &[&str]| {
        let start = Instant::now();
        let out = Command::new(env!("CARGO_BIN_EXE_iuredesk"))
            .args(args)
            .current_dir(work.path())
            .env("IUREDESK_DATA_DIR", data.path())
            .env("XDG_RUNTIME_DIR", runtime.path())
            .output()
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(20), "no debe abrir otra ventana");
        out
    };
    let out = run(&["factura.eml", "mailto:ana@example.com", "notas.txt"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("notas.txt"), "lo que no sabe abrir lo dice");
    let out = run(&[]);
    assert!(out.status.success());
    let got = got.lock().unwrap().clone();
    let eml = work.path().join("factura.eml");
    assert_eq!(got, vec![vec![eml.to_string_lossy().into_owned(), "mailto:ana@example.com".into()], vec![]]);
    drop(bridge);
}
