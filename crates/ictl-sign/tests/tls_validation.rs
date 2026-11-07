//! 证书校验必须失败关闭：自签证书被拒绝；只有显式加入的那张根证书能通过。
//! 不访问 Apple，也不访问公网。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread;

use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

#[tokio::test]
async fn untrusted_certificate_is_rejected_and_an_added_root_is_required() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.serialize_der().unwrap();
    let key_der = cert.serialize_private_key_der();
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert_der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
        )
        .unwrap();
    let server_config = Arc::new(server_config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut sock, _)) = listener.accept() else { break };
            let Ok(mut conn) = rustls::ServerConnection::new(server_config.clone()) else { break };
            let mut tls = rustls::Stream::new(&mut conn, &mut sock);
            let mut buf = [0u8; 2048];
            let _ = tls.read(&mut buf);
            let _ = tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });

    let url = format!("https://localhost:{}/", addr.port());
    let plain = omnisette::validated_http_client_builder()
        .timeout(std::time::Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let rejected = plain.get(&url).send().await.unwrap_err();
    let rejected = error_chain(&rejected);
    let lower = rejected.to_ascii_lowercase();
    assert!(
        lower.contains("certificate")
            || lower.contains("cert")
            || lower.contains("unknownissuer")
            || lower.contains("invalidpeer")
            || lower.contains("handshake"),
        "expected certificate failure, got {rejected}"
    );

    let trusted_root = reqwest::Certificate::from_der(&cert_der).unwrap();
    let trusting = omnisette::validated_http_client_builder()
        .add_root_certificate(trusted_root)
        .timeout(std::time::Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let response = trusting.get(&url).send().await.expect("explicitly trusted root should succeed");
    assert!(response.status().is_success(), "{}", response.status());
    let _ = server.join();
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(src) = cur {
        out.push_str(" | ");
        out.push_str(&src.to_string());
        cur = src.source();
    }
    out
}

#[test]
fn source_has_no_certificate_validation_bypass() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for rel in [
        "vendor/omnisette/src/remote_anisette_v3.rs",
        "vendor/omnisette/src/adi_proxy.rs",
        "vendor/omnisette/src/lib.rs",
        "vendor/plume_core/src/lib.rs",
    ] {
        let text = std::fs::read_to_string(root.join(rel)).unwrap();
        for line in text.lines() {
            let code = line.split("//").next().unwrap_or("");
            assert!(!code.contains("danger_accept_invalid_certs"), "{rel} reintroduced a TLS bypass: {line}");
        }
    }
}
