//! Live S3 adapter tests (Part 2 · S4-docs) against a REAL MinIO server:
//!
//! ```sh
//! docker run -d -p 9000:9000 -e MINIO_ROOT_USER=sutraci -e MINIO_ROOT_PASSWORD=sutraci-minio-secret \
//!   minio/minio:RELEASE.2025-09-07T16-13-09Z server /data
//! SUTRA_TEST_S3_ENDPOINT=http://127.0.0.1:9000 cargo test -p sutra-infra --features s3-tests --test live_s3
//! ```
//!
//! Covers: bucket creation + privacy proof, signed PUT / GET round trip, size cap on read,
//! delete, anonymous access refused (object and listing), presigned GET (valid, then expired),
//! a bucket made public is refused for every operation.
#![cfg(feature = "s3-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;
use sutra_infra::s3::{S3Client, S3Settings, ServerSideEncryption};
use sutra_kernel::error::KernelError;
use sutra_kernel::secret::sha256;

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn client(bucket: &str) -> S3Client {
    S3Client::new(S3Settings {
        endpoint: env("SUTRA_TEST_S3_ENDPOINT", "http://127.0.0.1:9000"),
        public_endpoint: None,
        region: env("SUTRA_TEST_S3_REGION", "us-east-1"),
        bucket: bucket.to_owned(),
        access_key_id: env("SUTRA_TEST_S3_ACCESS_KEY", "sutraci"),
        secret_access_key: env("SUTRA_TEST_S3_SECRET_KEY", "sutraci-minio-secret"),
        path_style: true,
        sse: ServerSideEncryption::None,
        sse_kms_key_id: None,
        create_bucket: true,
        timeout: Duration::from_secs(10),
    })
    .unwrap()
}

fn key(name: &str) -> String {
    format!(
        "{}/live/{}/{name}",
        uuid::Uuid::now_v7(),
        uuid::Uuid::now_v7()
    )
}

#[tokio::test]
async fn round_trip_delete_and_size_cap() {
    let c = client("sutra-it-live");
    c.ensure_ready().await.unwrap();
    c.health().await.unwrap();
    let k = key("a.pdf");
    let body = b"%PDF-1.7\n live round trip \n%%EOF\n".to_vec();
    c.put_object(&k, "application/pdf", body.clone(), &sha256(&body))
        .await
        .unwrap();
    assert_eq!(c.get_object(&k, 1024).await.unwrap(), body);
    assert!(matches!(
        c.get_object(&k, 4).await,
        Err(KernelError::Conflict(_))
    ));
    // The local hash check refuses a body that does not match the claimed digest.
    assert!(c
        .put_object(&k, "application/pdf", b"other".to_vec(), &sha256(&body))
        .await
        .is_err());
    c.delete_object(&k).await.unwrap();
    assert!(matches!(
        c.get_object(&k, 1024).await,
        Err(KernelError::NotFound { .. })
    ));
    c.delete_object(&k).await.unwrap(); // idempotent
}

#[tokio::test]
async fn anonymous_access_is_refused_and_presigned_urls_expire() {
    let c = client("sutra-it-live");
    let k = key("b.png");
    let body = b"\x89PNG\r\n\x1a\nlive".to_vec();
    c.put_object(&k, "image/png", body.clone(), &sha256(&body))
        .await
        .unwrap();
    let base = env("SUTRA_TEST_S3_ENDPOINT", "http://127.0.0.1:9000");
    let http = reqwest::Client::new();
    // Direct key guessing without credentials: object and listing are refused.
    let anon = http
        .get(format!("{base}/sutra-it-live/{k}"))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), 403, "anonymous object read must be refused");
    let list = http
        .get(format!("{base}/sutra-it-live?list-type=2"))
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 403, "anonymous listing must be refused");
    // A presigned URL works for its lifetime only.
    let url = c
        .presign_get(
            &k,
            Duration::from_secs(3),
            Some("attachment; filename=\"b.png\""),
            Some("image/png"),
        )
        .unwrap();
    let ok = http.get(&url).send().await.unwrap();
    assert_eq!(ok.status(), 200);
    assert_eq!(
        ok.headers()["content-disposition"],
        "attachment; filename=\"b.png\""
    );
    assert_eq!(ok.bytes().await.unwrap().to_vec(), body);
    tokio::time::sleep(Duration::from_millis(5000)).await;
    let expired = http.get(&url).send().await.unwrap();
    assert_eq!(expired.status(), 403, "an expired presigned URL is refused");
    c.delete_object(&k).await.unwrap();
}

#[tokio::test]
async fn a_public_bucket_is_refused() {
    let id = uuid::Uuid::now_v7().simple().to_string();
    let bucket = format!("sutra-it-pub-{}", &id[id.len() - 12..]);
    let bucket = bucket.as_str();
    let admin = client(bucket);
    admin.ensure_ready().await.unwrap();
    let policy = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Principal": {"AWS": ["*"]}, "Action": ["s3:GetObject"],
             "Resource": [format!("arn:aws:s3:::{bucket}/*")]},
            {"Effect": "Allow", "Principal": {"AWS": ["*"]}, "Action": ["s3:ListBucket"],
             "Resource": [format!("arn:aws:s3:::{bucket}")]}
        ]
    });
    admin.put_bucket_policy(&policy.to_string()).await.unwrap();
    let fresh = client(bucket);
    let err = fresh.ensure_ready().await.unwrap_err();
    assert!(
        err.to_string().contains("public buckets are refused"),
        "{err}"
    );
    let body = b"%PDF-1.7 x".to_vec();
    assert!(fresh
        .put_object(&key("c.pdf"), "application/pdf", body.clone(), &sha256(&body))
        .await
        .is_err());
}
