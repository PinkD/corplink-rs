//! HTTPS policy shared by discovery, authentication, and gateway probes.
use anyhow::{bail, Context, Result};
use reqwest::{Certificate, ClientBuilder, Url};
use std::net::IpAddr;
use std::time::Duration;

pub fn validate_server_url(value: &str) -> Result<Url> {
    // URL parsers otherwise silently strip some control characters.
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("server URL must not contain whitespace or control characters");
    }
    let url = Url::parse(value).context("invalid server URL")?;
    if url.scheme() != "https" || url.host_str().is_none() {
        bail!("server URL must use HTTPS and include a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("server URL must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() || url.port() == Some(0) {
        bail!("server URL must not contain a query, fragment, or zero port");
    }
    Ok(url)
}

pub fn vpn_endpoint_url(host: &str, port: u16) -> Result<Url> {
    if port == 0 {
        bail!("VPN control port must be nonzero");
    }
    let mut url = Url::parse("https://localhost").expect("static HTTPS URL");
    if let Ok(ip) = host.parse::<IpAddr>() {
        url.set_ip_host(ip)
            .map_err(|_| anyhow::anyhow!("invalid VPN IP address"))?;
    } else {
        // Accept DNS names, never URLs or UAPI lines. IP literals are parsed above.
        if host.is_empty()
            || host.len() > 253
            || !host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            bail!("invalid VPN gateway hostname");
        }
        url.set_host(Some(host))
            .context("invalid VPN gateway hostname")?;
    }
    url.set_port(Some(port))
        .map_err(|_| anyhow::anyhow!("invalid VPN port"))?;
    Ok(url)
}

pub fn client_builder(ca_cert_file: Option<&str>) -> Result<ClientBuilder> {
    let mut builder = ClientBuilder::new()
        .https_only(true)
        // Custom CSRF/JWT headers must not follow an authentication redirect to
        // another origin. Logout itself returns 302 and does not need following.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!(
            "CorpLink/{} (linux; Linux; en)",
            crate::api::CORPLINK_APP_VERSION
        ))
        .timeout(Duration::from_secs(10));
    if let Some(path) = ca_cert_file {
        let pem = std::fs::read(path).context("failed to read configured CA bundle")?;
        let certificates = Certificate::from_pem_bundle(&pem).context("invalid PEM CA bundle")?;
        if certificates.is_empty() {
            bail!("configured CA bundle contains no certificates");
        }
        for certificate in certificates {
            builder = builder.add_root_certificate(certificate);
        }
    }
    Ok(builder)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, OnceLock};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_rustls::{rustls, TlsAcceptor};

    // Fixtures answer only after receiving the complete request. A TLS read may
    // contain only part of a header/body; closing early can otherwise cause RST.
    pub async fn read_http_request<S: tokio::io::AsyncRead + Unpin>(
        stream: &mut S,
    ) -> std::io::Result<Vec<u8>> {
        use std::io::{Error, ErrorKind};
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = stream.read(&mut buffer).await?;
            if count == 0 {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "fixture request closed before completion",
                ));
            }
            request.extend_from_slice(&buffer[..count]);
            if request.len() > 64 * 1024 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "fixture request exceeded limit",
                ));
            }
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..end])
                    .map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
                let body_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>())
                    })
                    .transpose()
                    .map_err(|error| Error::new(ErrorKind::InvalidData, error))?
                    .unwrap_or(0);
                let total = (end + 4)
                    .checked_add(body_length)
                    .filter(|length| *length <= 64 * 1024)
                    .ok_or_else(|| {
                        Error::new(ErrorKind::InvalidData, "fixture request exceeded limit")
                    })?;
                if request.len() >= total {
                    return Ok(request);
                }
            }
        }
    }

    #[tokio::test]
    async fn request_fixture_consumes_fragmented_headers_and_body() {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        let request = b"POST /test HTTP/1.1\r\nContent-Length: 11\r\n\r\nhello world";
        let sending = tokio::spawn(async move {
            writer.write_all(request).await.unwrap();
        });
        assert_eq!(read_http_request(&mut reader).await.unwrap(), request);
        sending.await.unwrap();
    }

    #[tokio::test]
    async fn request_fixture_returns_connection_abort_and_truncated_body_errors() {
        struct AbortedRead;
        impl tokio::io::AsyncRead for AbortedRead {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    "simulated client certificate rejection",
                )))
            }
        }
        assert_eq!(
            read_http_request(&mut AbortedRead)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::ConnectionAborted
        );
        let (mut writer, mut reader) = tokio::io::duplex(64);
        writer
            .write_all(b"POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab")
            .await
            .unwrap();
        drop(writer);
        assert_eq!(
            read_http_request(&mut reader).await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    pub fn tls_fixture() -> &'static (String, TlsAcceptor) {
        static FIXTURE: OnceLock<(String, TlsAcceptor)> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let certified =
                rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
                    .unwrap();
            let key =
                rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
            let config = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![certified.cert.der().clone()], key.into())
            .unwrap();
            (certified.cert.pem(), TlsAcceptor::from(Arc::new(config)))
        })
    }

    #[test]
    fn rejects_plaintext_credentials_and_url_smuggling() {
        for url in [
            "http://vpn.example",
            "https://user:secret@vpn.example",
            "https://vpn.example\n",
            "https://vpn.example/?token=secret",
            "https://vpn.example/#secret",
            "https://vpn.example:0",
        ] {
            assert!(validate_server_url(url).is_err());
        }
        assert!(validate_server_url("https://vpn.example:8443/base").is_ok());
        for host in [
            "vpn.example\nprivate_key=secret",
            "vpn.example/path",
            "vpn.example@evil.test",
            "vpn.example%0a",
            "https://vpn.example",
            "",
        ] {
            assert!(vpn_endpoint_url(host, 443).is_err());
        }
        assert_eq!(
            vpn_endpoint_url("2001:db8::1", 443).unwrap().host_str(),
            Some("[2001:db8::1]")
        );
    }

    #[tokio::test]
    async fn self_signed_tls_requires_explicit_ca_and_plaintext_is_rejected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        );
        let acceptor = tls_fixture().1.clone();
        let server = tokio::spawn(async move {
            // The client sends rejected-CA, trusted-CA, then wrong-hostname
            // requests in order. Schannel can reject the certificate only after
            // the server's TLS accept succeeds, surfacing as a request-read error.
            for request_index in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut stream = match acceptor.accept(stream).await {
                    Ok(stream) => stream,
                    Err(error) => {
                        assert_ne!(
                            request_index, 1,
                            "trusted client's TLS handshake failed: {error}"
                        );
                        continue;
                    }
                };
                if let Err(error) = read_http_request(&mut stream).await {
                    assert_ne!(
                        request_index, 1,
                        "trusted client's HTTP request failed: {error}"
                    );
                    continue;
                }
                assert_eq!(
                    request_index, 1,
                    "client sent HTTP despite an untrusted certificate or hostname"
                );
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await
                    .unwrap();
            }
        });
        let untrusted = client_builder(None).unwrap().no_proxy().build().unwrap();
        assert!(untrusted.get(&url).send().await.is_err());
        let ca = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca.path(), &tls_fixture().0).unwrap();
        let trusted = client_builder(ca.path().to_str())
            .unwrap()
            .no_proxy()
            .build()
            .unwrap();
        assert_eq!(
            trusted
                .get(&url)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
        assert!(trusted
            .get(url.replacen("https:", "http:", 1))
            .send()
            .await
            .is_err());
        let address = url.parse::<Url>().unwrap().socket_addrs(|| None).unwrap()[0];
        let wrong_hostname = client_builder(ca.path().to_str())
            .unwrap()
            .no_proxy()
            .resolve("mismatch.invalid", address)
            .build()
            .unwrap();
        assert!(wrong_hostname
            .get(format!("https://mismatch.invalid:{}", address.port()))
            .send()
            .await
            .is_err());
        server.await.unwrap();
    }
}
