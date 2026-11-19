//! F8.5: un Exchange con certificado de la CA **interna** de la empresa. Sin la CA en el sistema,
//! EWS lo rechaza (certificado no válido, sin mandar la contraseña); con la CA instalada en el
//! sistema (aquí, `SSL_CERT_FILE` en un proceso hijo) funciona, porque `reqwest` confía también
//! en las raíces del sistema.

use std::sync::Arc;
use std::time::Duration;

use iuredesk_ews::{BasicAuth, EwsClient, EwsError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "../../net/tests/tls_lab/mod.rs"]
mod tls_lab;

/// EWS falso por HTTPS con el certificado de la CA interna.
async fn server() -> u16 {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_lab::server_config(&tls_lab::lab_dir()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut s) = acceptor.accept(tcp).await else { return };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                // Cabeceras y cuerpo (la petición es pequeña).
                while let Ok(n) = s.read(&mut tmp).await {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if String::from_utf8_lossy(&buf).contains("</soap:Envelope>") {
                        break;
                    }
                }
                let body = r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><m:SyncFolderHierarchyResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>CA1</m:SyncState><m:IncludesLastFolderInRange>true</m:IncludesLastFolderInRange><m:Changes/></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse></s:Body></s:Envelope>"#;
                let _ = s
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = s.shutdown().await;
            });
        }
    });
    port
}

async fn hierarchy(port: u16) -> Result<String, EwsError> {
    let url = format!("https://127.0.0.1:{port}/EWS/Exchange.asmx");
    let c = EwsClient::new(&url, Arc::new(BasicAuth::new("ana@iuredesk.test", "secreto")), Duration::from_secs(10))?;
    Ok(c.sync_folder_hierarchy(None).await?.state)
}

#[tokio::test]
#[ignore = "lo corre internal_ca_from_the_system con SSL_CERT_FILE"]
async fn child_with_internal_ca() {
    assert!(tls_lab::in_child(), "solo como proceso hijo");
    let port = server().await;
    assert_eq!(hierarchy(port).await.unwrap(), "CA1");
}

#[tokio::test]
async fn internal_ca_from_the_system() {
    let port = server().await;
    let err = hierarchy(port).await.unwrap_err();
    assert!(matches!(&err, EwsError::Network(m) if m.to_lowercase().contains("certificate")), "{err:?}");

    tls_lab::run_child("child_with_internal_ca", &tls_lab::lab_dir());
}
