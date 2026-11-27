use std::{net::SocketAddr, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        ServerConfig,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    },
};

pub(super) const WHEEL: &str = "fixture_package-1.0.0-py3-none-any.whl";
const TLS_FIXTURE: &[u8] = include_bytes!("https-fixture.pem");
const HOSTS: &[&str] = &[
    "general.test",
    "sub.general.test",
    "package.test",
    "shared.test",
    "unlisted.test",
    "pypi.org",
];

pub(super) struct HttpsFixture {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl HttpsFixture {
    pub(super) async fn start() -> Self {
        let certificate = CertificateDer::from_pem_slice(TLS_FIXTURE).unwrap();
        let key = PrivateKeyDer::from_pem_slice(TLS_FIXTURE).unwrap();
        let tls = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(stream).await {
                        serve(stream, addr.port()).await.unwrap();
                    }
                });
            }
        });
        Self { addr, task }
    }

    pub(super) fn url(&self, host: &str, path: &str) -> String {
        format!("https://{host}:{}{path}", self.addr.port())
    }

    pub(super) fn client_builder(&self) -> reqwest::ClientBuilder {
        let certificate = reqwest::Certificate::from_pem(TLS_FIXTURE).unwrap();
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(certificate);
        for host in HOSTS {
            builder = builder.resolve(host, self.addr);
        }
        builder
    }
}

impl Drop for HttpsFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    port: u16,
) -> std::io::Result<()> {
    let mut stream = BufReader::new(stream);
    let mut request = String::new();
    stream.read_line(&mut request).await?;
    let path = request.split_ascii_whitespace().nth(1).unwrap_or("");
    let response = match path {
        "/relative-redirect" => "302 Found\r\nLocation: /download".to_owned(),
        "/allowed-redirect" => {
            format!("302 Found\r\nLocation: https://sub.general.test:{port}/relative-redirect")
        }
        "/blocked-host" => format!("302 Found\r\nLocation: https://unlisted.test:{port}/download"),
        "/blocked-scheme" => format!("302 Found\r\nLocation: http://general.test:{port}/download"),
        "/initialization-redirect" => {
            format!("302 Found\r\nLocation: https://package.test:{port}/download")
        }
        "/download" => return write_body(stream, "text/plain", b"fixture data").await,
        path if path.strip_prefix('/') == Some(WHEEL) => {
            let wheel = STANDARD
                .decode(include_str!("fixture_package-1.0.0-py3-none-any.whl.base64").trim())
                .unwrap();
            return write_body(stream, "application/octet-stream", &wheel).await;
        }
        _ => "404 Not Found".to_owned(),
    };
    let headers = format!("HTTP/1.1 {response}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    stream.write_all(headers.as_bytes()).await?;
    stream.shutdown().await
}

async fn write_body(
    mut stream: BufReader<tokio_rustls::server::TlsStream<tokio::net::TcpStream>>,
    content_type: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(bytes).await?;
    stream.shutdown().await
}
