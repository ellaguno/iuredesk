//! F8.5: un servidor con certificado de una CA **interna** (la de la empresa, no pública).
//! Sin esa CA en el sistema la conexión se rechaza (certificado no válido); con la CA instalada
//! en el sistema (aquí, `SSL_CERT_FILE` en un proceso hijo) se acepta, porque IureDesk confía en
//! las raíces del sistema además de las de Mozilla.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod tls_lab;

/// Servidor TLS de un solo uso con el certificado firmado por la CA interna; responde «OK».
async fn server() -> u16 {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls_lab::server_config(&tls_lab::lab_dir()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            if let Ok(mut s) = acceptor.accept(tcp).await {
                let _ = s.write_all(b"OK").await;
                let _ = s.shutdown().await;
            }
        }
    });
    port
}

async fn read_ok(port: u16) -> Result<String, iuredesk_net::NetError> {
    let mut s = iuredesk_net::connect("127.0.0.1", port, true, Duration::from_secs(5)).await?;
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf).await;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// En el proceso hijo, con la CA interna instalada «en el sistema»: se conecta.
#[tokio::test]
#[ignore = "lo corre internal_ca_from_the_system con SSL_CERT_FILE"]
async fn child_with_internal_ca() {
    assert!(tls_lab::in_child(), "solo como proceso hijo");
    assert!(!iuredesk_net::native_roots().is_empty());
    let port = server().await;
    assert_eq!(read_ok(port).await.unwrap(), "OK");
}

#[tokio::test]
async fn internal_ca_from_the_system() {
    // Sin la CA (solo las raíces de Mozilla y las del equipo de pruebas): certificado no válido.
    let port = server().await;
    let err = read_ok(port).await.unwrap_err();
    assert!(matches!(err, iuredesk_net::NetError::TlsCertificate(_)), "{err:?}");

    // Con la CA en el «sistema» (SSL_CERT_FILE, que lee rustls-native-certs): se acepta.
    tls_lab::run_child("child_with_internal_ca", &tls_lab::lab_dir());
}
