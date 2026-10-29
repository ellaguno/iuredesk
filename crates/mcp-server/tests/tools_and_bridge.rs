//! Herramientas MCP sobre el buzón de ejemplo y puente IPC con la app abierta.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{TimeZone, Utc};
use chrono_tz::Tz;
use iuredesk_mcp::{BridgeClient, BridgeOptions, DeskMcp, ToolContext, ToolError, ToolOutput, dispatch, start_bridge};
use iuredesk_model::{FolderRole, MessageId};
use iuredesk_store::Store;
use serde_json::{Value, json};

fn ctx() -> ToolContext {
    ToolContext { now: Utc.with_ymd_and_hms(2026, 10, 26, 12, 0, 0).unwrap(), spanish: true, zone: Tz::UTC }
}

fn sample(dir: &Path) -> Store {
    let store = Store::open(dir).unwrap();
    iuredesk_store::sample::populate(&store, iuredesk_store::sample::DEFAULT_SEED, ctx().now).unwrap();
    store
}

fn data(out: Result<ToolOutput, ToolError>) -> Value {
    match out {
        Ok(ToolOutput::Data(v)) => v,
        other => panic!("se esperaban datos: {other:?}"),
    }
}

#[test]
fn herramientas_sobre_el_buzon_de_ejemplo() {
    let dir = tempfile::tempdir().unwrap();
    let store = sample(dir.path());
    let c = ctx();

    // buscar_correo → leer_hilo
    let r = data(dispatch(&store, "buscar_correo", &json!({"query": "Altamira", "limit": 5}), &c));
    let first = &r["results"][0];
    assert!(r["count"].as_u64().unwrap() >= 1, "{r}");
    assert!(first["subject"].as_str().is_some());
    let thread = first["thread_id"].as_u64().unwrap();
    let Ok(ToolOutput::Text(t)) = dispatch(&store, "leer_hilo", &json!({"thread_id": thread}), &c) else {
        panic!("leer_hilo no devolvió texto")
    };
    assert!(t.contains("Asunto/Subject:") && t.contains(first["subject"].as_str().unwrap()));
    let Ok(ToolOutput::Text(small)) = dispatch(&store, "leer_hilo", &json!({"thread_id": thread, "max_chars": 1}), &c)
    else {
        panic!()
    };
    assert!(small.chars().count() < 200 && small.contains("recortado"));
    // Por message_id, con prefijo de texto.
    let mid = first["id"].as_u64().unwrap();
    assert!(dispatch(&store, "leer_hilo", &json!({"message_id": format!("msg:{mid}")}), &c).is_ok());
    assert!(matches!(dispatch(&store, "leer_hilo", &json!({"thread_id": 999_999}), &c), Err(ToolError::Failed(_))));

    // crear_borrador: nuevo
    let ops_before = store.ops_len().unwrap();
    let r = data(dispatch(
        &store,
        "crear_borrador",
        &json!({"to": "Ana <ana@example.com>", "subject": "Propuesta", "body_markdown": "Hola **Ana**,\n\n- uno\n- dos"}),
        &c,
    ));
    assert_eq!(r["sent"], json!(false));
    let id = MessageId(r["draft_id"].as_u64().unwrap());
    let env = store.get_envelope(id).unwrap().unwrap();
    assert!(env.flags.draft && env.flags.seen);
    let folder = store.get_folder(env.folder).unwrap().unwrap();
    assert_eq!(folder.role, FolderRole::Drafts);
    let raw = String::from_utf8_lossy(&store.get_raw(id).unwrap()).into_owned();
    assert!(raw.contains("ana@example.com") && raw.contains("Propuesta"));
    let ops = store.peek_ops(1000).unwrap();
    assert_eq!(store.ops_len().unwrap(), ops_before + 1);
    assert!(ops.iter().any(|o| o.kind == "append_draft"));

    // crear_borrador: respuesta (cabeceras del hilo y cuenta del original)
    let orig = store.get_envelope(MessageId(mid)).unwrap().unwrap();
    let r = data(dispatch(
        &store,
        "crear_borrador",
        &json!({"reply_to_message_id": mid, "body_markdown": "De acuerdo."}),
        &c,
    ));
    let reply = store.get_envelope(MessageId(r["draft_id"].as_u64().unwrap())).unwrap().unwrap();
    assert_eq!(reply.account, orig.account);
    assert!(reply.subject.to_lowercase().starts_with("re"), "{}", reply.subject);
    assert_eq!(reply.in_reply_to.is_some(), orig.message_id.is_some());
    // Destinatario no válido: falla sin guardar nada.
    let n = store.message_count().unwrap();
    let bad = dispatch(&store, "crear_borrador", &json!({"to": "no es correo", "body_markdown": "x"}), &c);
    assert!(matches!(bad, Err(ToolError::Failed(_))));
    assert_eq!(store.message_count().unwrap(), n);

    // agenda de la semana del ejemplo
    let r = data(dispatch(&store, "agenda", &json!({"desde": "2026-10-26", "hasta": "2026-11-02"}), &c));
    assert!(r["count"].as_u64().unwrap() > 0, "{r}");
    assert!(matches!(
        dispatch(&store, "agenda", &json!({"desde": "2026-01-01", "hasta": "2026-12-31"}), &c),
        Err(ToolError::Failed(_))
    ));
    assert!(matches!(dispatch(&store, "agenda", &json!({"desde": "ayer"}), &c), Err(ToolError::Failed(_))));
    assert!(matches!(dispatch(&store, "agenda", &json!({"zona": "Marte/Olympus"}), &c), Err(ToolError::Failed(_))));

    // crear_evento → aparece en la agenda y queda en la cola de pim-sync
    let r = data(dispatch(
        &store,
        "crear_evento",
        &json!({"titulo": "Llamada con Ana", "inicio": "2026-10-28T17:00", "zona": "America/Mexico_City"}),
        &c,
    ));
    assert_eq!(r["start"], json!("2026-10-28T17:00:00-06:00"));
    let ag = data(dispatch(&store, "agenda", &json!({"desde": "2026-10-28", "hasta": "2026-10-29"}), &c));
    assert!(ag["events"].as_array().unwrap().iter().any(|e| e["title"] == "Llamada con Ana"));
    assert!(store.peek_ops(1000).unwrap().iter().any(|o| o.kind == "pim.event.put"));
    let r = data(dispatch(&store, "crear_evento", &json!({"titulo": "Puente", "inicio": "2026-11-02"}), &c));
    assert_eq!(r["all_day"], json!(true));
    assert_eq!(r["end"], json!("2026-11-03"));
    let back = dispatch(
        &store,
        "crear_evento",
        &json!({"titulo": "x", "inicio": "2026-11-02T10:00", "fin": "2026-11-02T09:00"}),
        &c,
    );
    assert!(matches!(back, Err(ToolError::Failed(_))));

    // contactos, notas, tareas
    let someone = store.find_contacts("", 1).unwrap().remove(0).full_name;
    let r = data(dispatch(&store, "buscar_contacto", &json!({"q": someone}), &c));
    assert!(r["count"].as_u64().unwrap() >= 1, "{r}");
    let r = data(dispatch(&store, "crear_nota", &json!({"title": "Ideas", "markdown": "# Ideas\n- a"}), &c));
    let nid = r["note_id"].as_u64().unwrap();
    assert!(store.notes().unwrap().iter().any(|n| n.id.get() == nid && n.body.starts_with("# Ideas")));
    let r = data(dispatch(&store, "mis_tareas", &json!({}), &c));
    let tasks = r["tasks"].as_array().unwrap();
    assert!(!tasks.is_empty());
    assert!(tasks.iter().all(|t| t["status"] != "Completed"));
    let all = data(dispatch(&store, "mis_tareas", &json!({"incluir_terminadas": true, "limit": 200}), &c));
    assert!(all["total"].as_u64() >= r["total"].as_u64());

    // Llamadas mal formadas
    assert!(matches!(dispatch(&store, "borrar_todo", &json!({}), &c), Err(ToolError::Params(_))));
    assert!(matches!(dispatch(&store, "buscar_correo", &json!([1]), &c), Err(ToolError::Params(_))));
    assert!(matches!(dispatch(&store, "buscar_correo", &json!({}), &c), Err(ToolError::Failed(_))));
}

#[test]
fn puente_con_la_app_abierta() {
    let dir = tempfile::tempdir().unwrap();
    let rt = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("store");
    let store = Arc::new(sample(&store_dir));
    let changes = Arc::new(AtomicUsize::new(0));
    let seen = changes.clone();
    let opts = BridgeOptions {
        runtime_dir: Some(rt.path().to_path_buf()),
        on_change: Some(Arc::new(move |_: &str| {
            seen.fetch_add(1, Ordering::SeqCst);
        })),
        context: Some(ctx()),
        ..Default::default()
    };
    let bridge = start_bridge(store.clone(), opts.clone()).unwrap();
    let endpoint = bridge.endpoint_file().to_path_buf();
    assert!(endpoint.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(bridge.endpoint_file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "el punto de acceso debe ser privado");
    }
    // Un segundo puente para el mismo almacén no arranca.
    assert!(start_bridge(store.clone(), opts).is_err());

    let mcp = DeskMcp::new(&store_dir).with_runtime_dir(rt.path()).with_context(ctx());
    let args = json!({"query": "Altamira", "limit": 3});
    let via_bridge = mcp.run("buscar_correo", &args).unwrap();
    assert_eq!(via_bridge, dispatch(&store, "buscar_correo", &args, &ctx()).unwrap());
    let ToolOutput::Data(found) = &via_bridge else { panic!("sin datos") };
    let thread = found["results"][0]["thread_id"].clone();
    assert!(matches!(mcp.run("leer_hilo", &json!({"thread_id": thread})), Ok(ToolOutput::Text(_))));
    // Los errores de la herramienta cruzan tal cual.
    assert!(matches!(mcp.run("nada", &json!({})), Err(ToolError::Params(_))));
    assert!(matches!(mcp.run("agenda", &json!({"desde": "x"})), Err(ToolError::Failed(_))));
    // Las escrituras avisan a la interfaz.
    assert_eq!(changes.load(Ordering::SeqCst), 0);
    mcp.run("crear_nota", &json!({"title": "Desde Claude", "markdown": "hola"})).unwrap();
    assert_eq!(changes.load(Ordering::SeqCst), 1);
    assert!(store.notes().unwrap().iter().any(|n| n.title == "Desde Claude"));

    // Sin puente pero con la app aún abierta (almacén bloqueado): error humano, sin colgarse.
    drop(bridge);
    assert!(!endpoint.exists(), "al cerrar se borra el punto de acceso");
    match mcp.run("buscar_correo", &args) {
        Err(ToolError::Failed(m)) => assert!(m.contains("IureDesk está abierto"), "{m}"),
        other => panic!("se esperaba un fallo: {other:?}"),
    }
    // App cerrada: el proceso MCP abre el almacén directamente (y lo suelta).
    drop(store);
    assert!(matches!(mcp.run("buscar_correo", &args), Ok(ToolOutput::Data(_))));
    assert!(matches!(mcp.run("mis_tareas", &json!({})), Ok(ToolOutput::Data(_))));
    // Un punto de acceso huérfano (la app murió sin borrarlo) no impide el acceso directo.
    std::fs::write(&endpoint, json!({"v": 1, "transport": "tcp", "addr": "127.0.0.1:9", "token": "x"}).to_string())
        .unwrap();
    assert!(matches!(mcp.run("buscar_contacto", &json!({"q": "Ana"})), Ok(ToolOutput::Data(_))));
}

#[test]
fn sin_datos_no_crea_el_almacen() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("no-existe");
    let mcp = DeskMcp::new(&store_dir).with_runtime_dir(dir.path()).with_context(ctx());
    assert!(matches!(mcp.run("mis_tareas", &json!({})), Err(ToolError::Failed(_))));
    assert!(!store_dir.exists());
}

/// R1.2: una segunda instancia pasa sus argumentos a la abierta por el puente (y nada más).
#[test]
fn segunda_instancia_reenvia_argumentos() {
    let dir = tempfile::tempdir().unwrap();
    let rt = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("store");
    let store = Arc::new(Store::open(&store_dir).unwrap());
    let got: Arc<std::sync::Mutex<Vec<Vec<String>>>> = Arc::default();
    let sink = got.clone();
    let opts = BridgeOptions {
        runtime_dir: Some(rt.path().to_path_buf()),
        on_open: Some(Arc::new(move |args: Vec<String>| sink.lock().unwrap().push(args))),
        ..Default::default()
    };
    let bridge = start_bridge(store.clone(), opts).unwrap();
    let client = BridgeClient::discover(&store_dir, Some(rt.path())).expect("punto de acceso");
    let args = vec!["/tmp/factura.eml".to_string(), "mailto:ana@example.com".to_string()];
    client.open(&args).unwrap();
    client.open(&[]).unwrap();
    // Hostil: demasiados y demasiado largos se recortan.
    let many: Vec<String> = (0..500).map(|i| format!("/tmp/{i}.ics")).chain(["x".repeat(20_000)]).collect();
    client.open(&many).unwrap();
    {
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], args);
        assert!(got[1].is_empty(), "sin argumentos = traer la ventana al frente");
        assert_eq!(got[2].len(), iuredesk_mcp::bridge::MAX_OPEN_ARGS);
    }
    // Con un token falso no entra nada.
    std::fs::write(
        bridge.endpoint_file(),
        std::fs::read_to_string(bridge.endpoint_file()).unwrap().replace("\"token\":\"", "\"token\":\"0"),
    )
    .unwrap();
    let forged = BridgeClient::discover(&store_dir, Some(rt.path())).unwrap();
    assert!(forged.open(&args).is_err());
    assert_eq!(got.lock().unwrap().len(), 3);
    // Sin `on_open`, la app no acepta archivos (y lo dice).
    drop(bridge);
    let bridge =
        start_bridge(store, BridgeOptions { runtime_dir: Some(rt.path().to_path_buf()), ..Default::default() })
            .unwrap();
    let client = BridgeClient::discover(&store_dir, Some(rt.path())).unwrap();
    assert!(matches!(client.open(&args), Err(iuredesk_mcp::bridge::BridgeCallError::Tool(_))));
    drop(bridge);
    // App cerrada: no contesta (el segundo proceso arranca normalmente).
    assert!(BridgeClient::discover(&store_dir, Some(rt.path())).is_none());
}
