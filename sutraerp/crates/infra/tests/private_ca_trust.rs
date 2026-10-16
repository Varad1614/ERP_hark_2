//! Part 2 · S5-deploy: outbound HTTPS trusts a private (college / internal) CA named by
//! `SSL_CERT_FILE`.
//!
//! An on-prem install runs MinIO (S3) and Keycloak behind certificates from a local CA, and
//! production refuses plain-http S3 (`config_docs::validate_production`). The adapters' reqwest
//! clients (S3, Keycloak, identity admin, Flowable, Razorpay, OpenSearch) are built with the
//! workspace `reqwest` features, which include `rustls-tls-native-roots`: the OS store, or the PEM
//! bundle named by `SSL_CERT_FILE`, is trusted alongside the bundled webpki roots.
//!
//! Needs only the `openssl` CLI (present on the CI runners): a CA and a `localhost` server
//! certificate are created, `openssl s_server -www` serves TLS, and a client built the way the
//! adapters build theirs REJECTS the server without the CA and ACCEPTS it once `SSL_CERT_FILE`
//! names the CA. The on-prem install workflow (`.github/workflows/sutraerp-part2-onprem.yml`)
//! proves the same end to end (document upload to MinIO over https, Keycloak over https).

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs `openssl <args>` (whitespace-separated, no quoting) in `dir`.
fn openssl(dir: &Path, args: &str) {
    let out = Command::new("openssl")
        .args(args.split_whitespace())
        .current_dir(dir)
        .output()
        .expect("the openssl CLI is required by this test");
    assert!(
        out.status.success(),
        "openssl {args} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// CA (`ca.crt`) + server certificate for `localhost` / 127.0.0.1 (`srv.crt`, `srv.key`).
fn make_pki(dir: &Path) {
    openssl(
        dir,
        "req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 2 \
         -subj /CN=sutra-test-private-ca \
         -addext basicConstraints=critical,CA:TRUE \
         -addext keyUsage=critical,keyCertSign,cRLSign",
    );
    openssl(dir, "req -newkey rsa:2048 -nodes -keyout srv.key -out srv.csr -subj /CN=localhost");
    std::fs::write(
        dir.join("srv.ext"),
        "basicConstraints=CA:FALSE\n\
         keyUsage=critical,digitalSignature,keyEncipherment\n\
         extendedKeyUsage=serverAuth\n\
         subjectAltName=DNS:localhost,IP:127.0.0.1\n",
    )
    .unwrap();
    openssl(
        dir,
        "x509 -req -in srv.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out srv.crt -days 2 \
         -extfile srv.ext",
    );
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn serve(dir: &Path, port: u16) -> Server {
    let accept = format!("-accept {port} -cert srv.crt -key srv.key -www -quiet");
    let child = Command::new("openssl")
        .arg("s_server")
        .args(accept.split_whitespace())
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn openssl s_server");
    let server = Server(child);
    let start = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "openssl s_server did not listen on {port}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    server
}

/// Built like the adapters' clients (`S3Client::new`, `KeycloakAuthenticator::new`, …).
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http client")
}

#[tokio::test]
async fn outbound_https_trusts_the_private_ca_in_ssl_cert_file() {
    let dir = std::env::temp_dir().join(format!("sutra-private-ca-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    make_pki(&dir);
    let port = free_port();
    let _server = serve(&dir, port);
    let url = format!("https://localhost:{port}/");

    // Without the CA: the private certificate is not trusted (webpki + system roots only).
    std::env::remove_var("SSL_CERT_FILE");
    let refused = client().get(&url).send().await;
    assert!(
        refused.is_err(),
        "a server certificate from an unknown private CA must be refused, got {refused:?}"
    );

    // SSL_CERT_FILE = the college / internal CA bundle: the same server is trusted.
    std::env::set_var("SSL_CERT_FILE", dir.join("ca.crt"));
    let res = client()
        .get(&url)
        .send()
        .await
        .expect("the private CA named by SSL_CERT_FILE must be trusted");
    assert!(res.status().is_success(), "openssl s_server -www answered {}", res.status());
    std::env::remove_var("SSL_CERT_FILE");
    let _ = std::fs::remove_dir_all(&dir);
}
