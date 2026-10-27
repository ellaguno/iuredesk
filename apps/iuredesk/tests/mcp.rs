//! `iuredesk --mcp` por stdio: `initialize` + `tools/list` sobre un almacén vacío (sin ventana
//! abierta: el proceso abre el almacén solo durante cada llamada).

use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

fn send(stdin: &mut ChildStdin, v: serde_json::Value) {
    writeln!(stdin, "{v}").unwrap();
    stdin.flush().unwrap();
}

fn wait_for(rx: &Receiver<String>, id: i64) -> serde_json::Value {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(200))
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
            && v.get("id").and_then(serde_json::Value::as_i64) == Some(id)
        {
            return v;
        }
    }
    panic!("sin respuesta a la petición {id}");
}

#[test]
fn mcp_lists_tools_over_stdio() {
    let data = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_iuredesk"))
        .arg("--mcp")
        .env("IUREDESK_DATA_DIR", data.path())
        .env("XDG_RUNTIME_DIR", runtime.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    send(
        &mut stdin,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"prueba","version":"1"}}}),
    );
    let init = wait_for(&rx, 1);
    assert!(init.get("result").is_some(), "{init}");
    send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    send(&mut stdin, serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let list = wait_for(&rx, 2);
    let names: Vec<String> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect();
    for want in ["buscar_correo", "leer_hilo", "crear_borrador", "agenda", "crear_evento", "mis_tareas"] {
        assert!(names.iter().any(|n| n == want), "falta {want}: {names:?}");
    }
    drop(stdin);
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) && child.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}
