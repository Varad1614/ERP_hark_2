//! S3-compatible object storage client — **Part 2 · Stage 4.2 (S4-docs)**.
//!
//! Plain REST over `reqwest` (rustls) with AWS Signature Version 4 implemented here on the
//! kernel's audited SHA-256 / HMAC helpers, so the adapter adds **no new crate** to the workspace
//! (no SDK, nothing new for cargo-deny). It speaks to MinIO on a college's own server (path-style
//! addressing, custom endpoint) and to AWS S3 (virtual-hosted or path-style, SSE-S3 / SSE-KMS).
//!
//! * Every request is signed with the **real payload hash** (`x-amz-content-sha256`), so the
//!   server rejects a body that was altered in transit (`XAmzContentSHA256Mismatch`).
//! * The SHA-256 of the object is also stored as user metadata (`x-amz-meta-sha256`).
//! * Redirects are never followed (a signed request is not replayed to another host).
//! * Before first use the bucket is checked (created only when `create_bucket` is set — dev /
//!   CI) and **proved private**: an anonymous GET of a probe object and an anonymous listing must
//!   both be refused. A bucket that answers anonymous reads is refused for every operation.
//! * Presigned GET URLs (query-string SigV4, `UNSIGNED-PAYLOAD`) for short-lived downloads; the
//!   caller authorises first.
//!
//! Domain modules never see this type: the composition root wraps it as the
//! `sutra_contracts::platform::documents::ObjectStorePort` (this crate depends only on the kernel).
//!
//! Signature V4 reference: <https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>
//! and <https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sigv4-query-string-auth.html>
//! (both published examples are unit tests below).

use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use sutra_kernel::error::KernelError;
use sutra_kernel::secret::{hex, hmac_sha256, sha256};

/// `x-amz-content-sha256` of an empty body.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// Payload hash of presigned URLs.
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
const ALGORITHM: &str = "AWS4-HMAC-SHA256";
/// Object written by the privacy probe (never a document key: those start with a tenant id).
pub const PROBE_KEY: &str = "_sutra/privacy-probe";
/// Longest presigned URL lifetime S3 accepts (7 days).
pub const MAX_PRESIGN_SECS: u64 = 604_800;

/// Server-side encryption requested on every PUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerSideEncryption {
    /// No SSE header (MinIO without a KMS; the disk / volume encryption of the server applies).
    None,
    /// `x-amz-server-side-encryption: AES256` (SSE-S3; MinIO needs a configured KMS).
    Aes256,
    /// `x-amz-server-side-encryption: aws:kms` (+ the key id when configured).
    Kms,
}

/// Connection settings (`Config::documents`).
#[derive(Clone)]
pub struct S3Settings {
    /// `http(s)://host[:port]` the API talks to (MinIO: `http://minio:9000`).
    pub endpoint: String,
    /// Browser-facing endpoint used only in presigned URLs (defaults to `endpoint`).
    pub public_endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Path-style addressing (`/<bucket>/<key>`; MinIO). `false` = virtual-hosted (`<bucket>.host`).
    pub path_style: bool,
    pub sse: ServerSideEncryption,
    pub sse_kms_key_id: Option<String>,
    /// Create the bucket when it is missing (development / CI only).
    pub create_bucket: bool,
    /// Per-request timeout.
    pub timeout: Duration,
}

impl std::fmt::Debug for S3Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Settings")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("path_style", &self.path_style)
            .field("sse", &self.sse)
            .finish_non_exhaustive()
    }
}

/// SigV4 credentials + scope region (no `Debug`: it holds the secret).
#[derive(Clone, Copy)]
pub struct Credentials<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub region: &'a str,
}

fn unavailable(e: impl std::fmt::Display) -> KernelError {
    KernelError::Unavailable(format!("object storage: {e}"))
}

/// RFC 3986 encoding as SigV4 defines it (unreserved characters kept, `%XX` upper case,
/// `/` kept only when `encode_slash` is false — object key paths).
pub fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(b));
            }
            b'/' if !encode_slash => out.push('/'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

/// Canonical query string: names and values encoded, sorted by name (then value).
pub fn canonical_query(params: &[(String, String)]) -> String {
    let mut enc: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
        .collect();
    enc.sort();
    enc.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Proleptic Gregorian date of a day count since 1970-01-01 (H. Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYYMMDDTHHMMSSZ` (UTC) of `t`.
pub fn amz_date(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// AWS Signature V4 for S3. `headers` are the signed headers: lower-case names, sorted by name.
/// Returns (`SignedHeaders`, hex signature).
#[allow(clippy::too_many_arguments)]
pub fn sign(
    cred: &Credentials<'_>,
    amz_date: &str,
    method: &str,
    canonical_uri: &str,
    canonical_query: &str,
    headers: &[(String, String)],
    payload_hash: &str,
) -> Result<(String, String), KernelError> {
    let date = amz_date
        .get(..8)
        .ok_or_else(|| KernelError::internal_msg("amz date is malformed"))?;
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let mut canonical_headers = String::new();
    for (k, v) in headers {
        canonical_headers.push_str(k);
        canonical_headers.push(':');
        canonical_headers.push_str(v.trim());
        canonical_headers.push('\n');
    }
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let scope = format!("{date}/{}/s3/aws4_request", cred.region);
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{scope}\n{}",
        hex(&sha256(canonical_request.as_bytes()))
    );
    let k_date = hmac_sha256(
        format!("AWS4{}", cred.secret_access_key).as_bytes(),
        date.as_bytes(),
    )?;
    let k_region = hmac_sha256(&k_date, cred.region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, b"s3")?;
    let k_signing = hmac_sha256(&k_service, b"aws4_request")?;
    let signature = hex(&hmac_sha256(&k_signing, string_to_sign.as_bytes())?);
    Ok((signed_headers, signature))
}

/// Where one request goes.
struct Target {
    scheme: String,
    host: String,
    canonical_uri: String,
}

impl Target {
    fn url(&self, query: &str) -> Result<reqwest::Url, KernelError> {
        let raw = if query.is_empty() {
            format!("{}://{}{}", self.scheme, self.host, self.canonical_uri)
        } else {
            format!(
                "{}://{}{}?{query}",
                self.scheme, self.host, self.canonical_uri
            )
        };
        reqwest::Url::parse(&raw).map_err(unavailable)
    }
}

fn parse_endpoint(raw: &str, what: &str) -> Result<url::Url, String> {
    let u = url::Url::parse(raw.trim()).map_err(|e| format!("{what} `{raw}`: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(format!("{what} must be http(s)://host[:port]"));
    }
    if u.host_str().is_none() || u.query().is_some() || u.fragment().is_some() {
        return Err(format!(
            "{what} must be http(s)://host[:port] without query"
        ));
    }
    Ok(u)
}

/// The first XML element `<tag>…</tag>` of an S3 error body.
fn xml_tag<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    body.get(start..end)
}

/// S3 REST client (one bucket).
pub struct S3Client {
    http: reqwest::Client,
    anonymous: reqwest::Client,
    settings: S3Settings,
    endpoint: url::Url,
    public_endpoint: url::Url,
    /// Set once the bucket exists and was proved private.
    verified: tokio::sync::OnceCell<()>,
}

impl std::fmt::Debug for S3Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Client")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl S3Client {
    pub fn new(settings: S3Settings) -> Result<Self, String> {
        let endpoint = parse_endpoint(&settings.endpoint, "S3 endpoint")?;
        let public_endpoint = match &settings.public_endpoint {
            Some(p) => parse_endpoint(p, "S3 public endpoint")?,
            None => endpoint.clone(),
        };
        if settings.bucket.len() < 3
            || settings.bucket.len() > 63
            || !settings
                .bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err(format!(
                "S3 bucket `{}` is not a valid bucket name (3-63 of a-z 0-9 . -)",
                settings.bucket
            ));
        }
        if settings.region.trim().is_empty() {
            return Err("S3 region is empty".into());
        }
        if settings.access_key_id.is_empty() || settings.secret_access_key.is_empty() {
            return Err("S3 access key id / secret access key are required".into());
        }
        let build = || {
            reqwest::Client::builder()
                .timeout(settings.timeout)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| format!("S3 http client: {e}"))
        };
        Ok(Self {
            http: build()?,
            anonymous: build()?,
            endpoint,
            public_endpoint,
            settings,
            verified: tokio::sync::OnceCell::new(),
        })
    }

    pub fn settings(&self) -> &S3Settings {
        &self.settings
    }

    fn credentials(&self) -> Credentials<'_> {
        Credentials {
            access_key_id: &self.settings.access_key_id,
            secret_access_key: &self.settings.secret_access_key,
            region: &self.settings.region,
        }
    }

    fn target(&self, base: &url::Url, key: Option<&str>) -> Result<Target, KernelError> {
        let host = base
            .host_str()
            .ok_or_else(|| unavailable("endpoint has no host"))?;
        let host = match base.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_owned(),
        };
        let prefix = base.path().trim_end_matches('/');
        let key_path = key
            .map(|k| format!("/{}", uri_encode(k, false)))
            .unwrap_or_default();
        let (host, canonical_uri) = if self.settings.path_style {
            (
                host,
                format!(
                    "{prefix}/{}{key_path}",
                    uri_encode(&self.settings.bucket, true)
                ),
            )
        } else {
            let uri = if key_path.is_empty() {
                format!("{prefix}/")
            } else {
                format!("{prefix}{key_path}")
            };
            (format!("{}.{host}", self.settings.bucket), uri)
        };
        Ok(Target {
            scheme: base.scheme().to_owned(),
            host,
            canonical_uri,
        })
    }

    /// One signed request against the API endpoint.
    async fn send(
        &self,
        method: reqwest::Method,
        key: Option<&str>,
        query: &[(String, String)],
        extra_headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
        payload_hash: &str,
    ) -> Result<reqwest::Response, KernelError> {
        let target = self.target(&self.endpoint, key)?;
        let date = amz_date(SystemTime::now());
        let query = canonical_query(query);
        let mut headers: Vec<(String, String)> = vec![
            ("host".to_owned(), target.host.clone()),
            ("x-amz-content-sha256".to_owned(), payload_hash.to_owned()),
            ("x-amz-date".to_owned(), date.clone()),
        ];
        headers.extend(extra_headers);
        headers.sort();
        let cred = self.credentials();
        let (signed, signature) = sign(
            &cred,
            &date,
            method.as_str(),
            &target.canonical_uri,
            &query,
            &headers,
            payload_hash,
        )?;
        let authorization = format!(
            "{ALGORITHM} Credential={}/{}/{}/s3/aws4_request, SignedHeaders={signed}, Signature={signature}",
            cred.access_key_id,
            date.get(..8).unwrap_or_default(),
            cred.region
        );
        let mut req = self.http.request(method, target.url(&query)?);
        for (k, v) in &headers {
            if k != "host" {
                req = req.header(k.as_str(), v.as_str());
            }
        }
        req = req.header("authorization", authorization);
        if let Some(b) = body {
            req = req.body(b);
        }
        req.send().await.map_err(|e| unavailable(e.without_url()))
    }

    async fn failure(resp: reqwest::Response, what: &str) -> KernelError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let code = xml_tag(&body, "Code").unwrap_or("").to_owned();
        if status == reqwest::StatusCode::NOT_FOUND && code != "NoSuchBucket" {
            return KernelError::NotFound {
                entity: "stored object",
            };
        }
        unavailable(format!("{what} answered {status} {code}"))
    }

    /// The bucket exists (created when allowed) and refuses anonymous reads. Checked once per
    /// process; a failure is not cached, so the next operation checks again.
    pub async fn ensure_ready(&self) -> Result<(), KernelError> {
        self.verified
            .get_or_try_init(|| async { self.verify_bucket().await })
            .await
            .map(|_| ())
    }

    async fn verify_bucket(&self) -> Result<(), KernelError> {
        let head = self
            .send(reqwest::Method::HEAD, None, &[], vec![], None, EMPTY_SHA256)
            .await?;
        match head.status().as_u16() {
            200 => {}
            404 if self.settings.create_bucket => self.create_bucket().await?,
            s => {
                return Err(unavailable(format!(
                    "bucket `{}` is not usable (HEAD answered {s})",
                    self.settings.bucket
                )))
            }
        }
        // Privacy proof: write a probe object, then read it and list the bucket anonymously.
        let probe = b"sutraerp document storage privacy probe".to_vec();
        let digest = sha256(&probe);
        self.put_unchecked(PROBE_KEY, "text/plain", probe, &digest)
            .await?;
        let object = self.target(&self.endpoint, Some(PROBE_KEY))?.url("")?;
        let listing = self
            .target(&self.endpoint, None)?
            .url("list-type=2&max-keys=1")?;
        for (what, url) in [("object read", object), ("listing", listing)] {
            let status = self
                .anonymous
                .get(url)
                .send()
                .await
                .map_err(|e| unavailable(e.without_url()))?
                .status();
            if status.is_success() {
                tracing::error!(
                    bucket = %self.settings.bucket,
                    "object storage bucket allows anonymous {what}: refusing to store documents"
                );
                return Err(unavailable(format!(
                    "bucket `{}` allows anonymous {what}; public buckets are refused",
                    self.settings.bucket
                )));
            }
        }
        tracing::info!(bucket = %self.settings.bucket, "object storage bucket is private");
        Ok(())
    }

    async fn create_bucket(&self) -> Result<(), KernelError> {
        let body = if self.settings.region == "us-east-1" {
            Vec::new()
        } else {
            format!(
                "<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 <LocationConstraint>{}</LocationConstraint></CreateBucketConfiguration>",
                self.settings.region
            )
            .into_bytes()
        };
        let hash = hex(&sha256(&body));
        let resp = self
            .send(reqwest::Method::PUT, None, &[], vec![], Some(body), &hash)
            .await?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::CONFLICT {
            tracing::info!(bucket = %self.settings.bucket, "object storage bucket created");
            Ok(())
        } else {
            Err(Self::failure(resp, "create bucket").await)
        }
    }

    async fn put_unchecked(
        &self,
        key: &str,
        content_type: &str,
        body: Vec<u8>,
        sha: &[u8; 32],
    ) -> Result<(), KernelError> {
        if sha256(&body) != *sha {
            return Err(KernelError::internal_msg(
                "object body does not match its SHA-256",
            ));
        }
        let hash = hex(sha);
        let mut headers = vec![
            ("content-type".to_owned(), content_type.to_owned()),
            ("x-amz-meta-sha256".to_owned(), hash.clone()),
        ];
        match self.settings.sse {
            ServerSideEncryption::None => {}
            ServerSideEncryption::Aes256 => headers.push((
                "x-amz-server-side-encryption".to_owned(),
                "AES256".to_owned(),
            )),
            ServerSideEncryption::Kms => {
                headers.push((
                    "x-amz-server-side-encryption".to_owned(),
                    "aws:kms".to_owned(),
                ));
                if let Some(k) = &self.settings.sse_kms_key_id {
                    headers.push((
                        "x-amz-server-side-encryption-aws-kms-key-id".to_owned(),
                        k.clone(),
                    ));
                }
            }
        }
        let resp = self
            .send(
                reqwest::Method::PUT,
                Some(key),
                &[],
                headers,
                Some(body),
                &hash,
            )
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(Self::failure(resp, "put object").await)
        }
    }

    /// Stores `body` under `key`. `sha` must be the SHA-256 of `body`; it is sent as the signed
    /// payload hash, so the server verifies the bytes it received.
    pub async fn put_object(
        &self,
        key: &str,
        content_type: &str,
        body: Vec<u8>,
        sha: &[u8; 32],
    ) -> Result<(), KernelError> {
        self.ensure_ready().await?;
        self.put_unchecked(key, content_type, body, sha).await
    }

    /// Reads `key`, refusing objects larger than `max_bytes`.
    pub async fn get_object(&self, key: &str, max_bytes: u64) -> Result<Vec<u8>, KernelError> {
        self.ensure_ready().await?;
        let mut resp = self
            .send(
                reqwest::Method::GET,
                Some(key),
                &[],
                vec![],
                None,
                EMPTY_SHA256,
            )
            .await?;
        if !resp.status().is_success() {
            return Err(Self::failure(resp, "get object").await);
        }
        if resp.content_length().is_some_and(|n| n > max_bytes) {
            return Err(KernelError::Conflict(
                "stored object is larger than the document size limit".into(),
            ));
        }
        let cap = usize::try_from(max_bytes).unwrap_or(usize::MAX);
        let mut out = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| unavailable(e.without_url()))?
        {
            if out.len() + chunk.len() > cap {
                return Err(KernelError::Conflict(
                    "stored object is larger than the document size limit".into(),
                ));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    /// Removes `key` (a missing object is not an error).
    pub async fn delete_object(&self, key: &str) -> Result<(), KernelError> {
        self.ensure_ready().await?;
        let resp = self
            .send(
                reqwest::Method::DELETE,
                Some(key),
                &[],
                vec![],
                None,
                EMPTY_SHA256,
            )
            .await?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(Self::failure(resp, "delete object").await)
        }
    }

    /// Sets the bucket policy (JSON). Operations tooling and the live tests use it; SutraERP
    /// itself never makes a bucket public (and refuses one that is, see [`Self::ensure_ready`]).
    pub async fn put_bucket_policy(&self, policy_json: &str) -> Result<(), KernelError> {
        let body = policy_json.as_bytes().to_vec();
        let hash = hex(&sha256(&body));
        let resp = self
            .send(
                reqwest::Method::PUT,
                None,
                &[("policy".to_owned(), String::new())],
                vec![("content-type".to_owned(), "application/json".to_owned())],
                Some(body),
                &hash,
            )
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(Self::failure(resp, "put bucket policy").await)
        }
    }

    /// Readiness: the bucket answers and was proved private.
    pub async fn health(&self) -> Result<(), KernelError> {
        self.ensure_ready().await?;
        let head = self
            .send(reqwest::Method::HEAD, None, &[], vec![], None, EMPTY_SHA256)
            .await?;
        if head.status().is_success() {
            Ok(())
        } else {
            Err(unavailable(format!(
                "bucket HEAD answered {}",
                head.status()
            )))
        }
    }

    /// Presigned GET of `key` valid for `ttl` from now (browser-facing endpoint).
    pub fn presign_get(
        &self,
        key: &str,
        ttl: Duration,
        response_content_disposition: Option<&str>,
        response_content_type: Option<&str>,
    ) -> Result<String, KernelError> {
        self.presign_get_at(
            key,
            ttl,
            SystemTime::now(),
            response_content_disposition,
            response_content_type,
        )
    }

    /// [`Self::presign_get`] at a fixed signing time (tests use the AWS example).
    pub fn presign_get_at(
        &self,
        key: &str,
        ttl: Duration,
        at: SystemTime,
        response_content_disposition: Option<&str>,
        response_content_type: Option<&str>,
    ) -> Result<String, KernelError> {
        let secs = ttl.as_secs();
        if secs == 0 || secs > MAX_PRESIGN_SECS {
            return Err(KernelError::internal_msg(
                "presigned URL lifetime must be 1 s .. 7 days",
            ));
        }
        let target = self.target(&self.public_endpoint, Some(key))?;
        let date = amz_date(at);
        let cred = self.credentials();
        let scope = format!(
            "{}/{}/s3/aws4_request",
            date.get(..8).unwrap_or_default(),
            cred.region
        );
        let mut params: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".into(), ALGORITHM.into()),
            (
                "X-Amz-Credential".into(),
                format!("{}/{scope}", cred.access_key_id),
            ),
            ("X-Amz-Date".into(), date.clone()),
            ("X-Amz-Expires".into(), secs.to_string()),
            ("X-Amz-SignedHeaders".into(), "host".into()),
        ];
        if let Some(d) = response_content_disposition {
            params.push(("response-content-disposition".into(), d.to_owned()));
        }
        if let Some(c) = response_content_type {
            params.push(("response-content-type".into(), c.to_owned()));
        }
        let query = canonical_query(&params);
        let headers = vec![("host".to_owned(), target.host.clone())];
        let (_, signature) = sign(
            &cred,
            &date,
            "GET",
            &target.canonical_uri,
            &query,
            &headers,
            UNSIGNED_PAYLOAD,
        )?;
        Ok(format!(
            "{}://{}{}?{query}&X-Amz-Signature={signature}",
            target.scheme, target.host, target.canonical_uri
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

    fn example_time() -> SystemTime {
        // Fri, 24 May 2013 00:00:00 GMT
        UNIX_EPOCH + Duration::from_secs(1_369_353_600)
    }

    fn settings(endpoint: &str, path_style: bool) -> S3Settings {
        S3Settings {
            endpoint: endpoint.into(),
            public_endpoint: None,
            region: "us-east-1".into(),
            bucket: "examplebucket".into(),
            access_key_id: KEY_ID.into(),
            secret_access_key: SECRET.into(),
            path_style,
            sse: ServerSideEncryption::None,
            sse_kms_key_id: None,
            create_bucket: false,
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn amz_date_is_utc_basic_format() {
        assert_eq!(amz_date(example_time()), "20130524T000000Z");
        assert_eq!(amz_date(UNIX_EPOCH), "19700101T000000Z");
        // 2028-02-29 23:59:59 UTC (leap day)
        assert_eq!(
            amz_date(UNIX_EPOCH + Duration::from_secs(1_835_481_599)),
            "20280229T235959Z"
        );
    }

    #[test]
    fn uri_encoding_follows_sigv4() {
        assert_eq!(uri_encode("a b/c~d", true), "a%20b%2Fc~d");
        assert_eq!(uri_encode("t/i/x_y/1-2.pdf", false), "t/i/x_y/1-2.pdf");
        assert_eq!(uri_encode("ä", true), "%C3%A4");
    }

    /// AWS example "GET Object" (sig-v4-header-based-auth): the published signature.
    #[test]
    fn header_signature_matches_the_aws_example() {
        let cred = Credentials {
            access_key_id: KEY_ID,
            secret_access_key: SECRET,
            region: "us-east-1",
        };
        let headers = vec![
            (
                "host".to_owned(),
                "examplebucket.s3.amazonaws.com".to_owned(),
            ),
            ("range".to_owned(), "bytes=0-9".to_owned()),
            ("x-amz-content-sha256".to_owned(), EMPTY_SHA256.to_owned()),
            ("x-amz-date".to_owned(), "20130524T000000Z".to_owned()),
        ];
        let (signed, sig) = sign(
            &cred,
            "20130524T000000Z",
            "GET",
            "/test.txt",
            "",
            &headers,
            EMPTY_SHA256,
        )
        .unwrap_or_default();
        assert_eq!(signed, "host;range;x-amz-content-sha256;x-amz-date");
        assert_eq!(
            sig,
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    /// AWS example presigned URL (sigv4-query-string-auth): byte-identical URL.
    #[test]
    fn presigned_url_matches_the_aws_example() {
        let c = S3Client::new(settings("https://s3.amazonaws.com", false)).ok();
        let url = c
            .as_ref()
            .and_then(|c| {
                c.presign_get_at(
                    "test.txt",
                    Duration::from_secs(86_400),
                    example_time(),
                    None,
                    None,
                )
                .ok()
            })
            .unwrap_or_default();
        assert_eq!(
            url,
            "https://examplebucket.s3.amazonaws.com/test.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host\
             &X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        );
    }

    #[test]
    fn path_style_targets_and_validation() {
        let c = S3Client::new(settings("http://minio:9000", true)).ok();
        let t = c
            .as_ref()
            .and_then(|c| c.target(&c.endpoint, Some("t/i/o/1/2")).ok());
        assert_eq!(
            t.as_ref()
                .map(|t| (t.host.as_str(), t.canonical_uri.as_str())),
            Some(("minio:9000", "/examplebucket/t/i/o/1/2"))
        );
        // Presigned URLs carry response overrides in the signed query.
        let url = c
            .as_ref()
            .and_then(|c| {
                c.presign_get(
                    "k",
                    Duration::from_secs(60),
                    Some("attachment; filename=\"a.pdf\""),
                    Some("application/pdf"),
                )
                .ok()
            })
            .unwrap_or_default();
        assert!(url.starts_with("http://minio:9000/examplebucket/k?X-Amz-Algorithm="));
        assert!(
            url.contains("&response-content-disposition=attachment%3B%20filename%3D%22a.pdf%22")
        );
        assert!(url.contains("X-Amz-Expires=60&"));
        for bad in ["ftp://x", "http://", "not a url", "http://h:9000/?a=b"] {
            assert!(S3Client::new(settings(bad, true)).is_err(), "{bad}");
        }
        let mut s = settings("http://minio:9000", true);
        s.bucket = "Bad_Bucket".into();
        assert!(S3Client::new(s).is_err());
        let ttl0 = c
            .as_ref()
            .map(|c| c.presign_get("k", Duration::ZERO, None, None).is_err());
        assert_eq!(ttl0, Some(true));
        assert!(!format!("{:?}", settings("http://m", true)).contains(SECRET));
    }

    #[test]
    fn s3_error_codes_are_read() {
        let body =
            "<?xml version=\"1.0\"?><Error><Code>AccessDenied</Code><Message>x</Message></Error>";
        assert_eq!(xml_tag(body, "Code"), Some("AccessDenied"));
        assert_eq!(xml_tag("nothing", "Code"), None);
    }
}
