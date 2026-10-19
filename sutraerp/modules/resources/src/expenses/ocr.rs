//! Receipt OCR behind a port, so the reader can be swapped (ADR: PaddleOCR chosen 2026-10-17;
//! Apache-2.0, self-hosted, so receipts and staff data never leave the institution's servers).
//!
//! * [`PaddleOcrHttp`] calls the sidecar in `tools/ocr-sidecar` (`POST {SUTRA_OCR_URL}/ocr`
//!   with the image as base64, answering text lines with confidences).
//! * [`DisabledOcr`] is used when `SUTRA_OCR_URL` is unset: uploads still work, nothing is
//!   pre-filled and the claimant types the fields.
//!
//! OCR only ever pre-fills a draft expense; a person confirms every field before it counts.

use super::receipt_text::OcrLine;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Why a receipt could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrError {
    /// No reader is configured (`SUTRA_OCR_URL` unset).
    Disabled,
    /// The reader is configured but failed or timed out.
    Unavailable(String),
}

impl std::fmt::Display for OcrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str("receipt reading is not configured"),
            Self::Unavailable(m) => write!(f, "the receipt reader failed: {m}"),
        }
    }
}

/// What a reader returns: lines top to bottom, and which engine read them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrText {
    pub lines: Vec<OcrLine>,
    #[serde(default)]
    pub engine: String,
}

/// Port: read the text of one receipt image (JPEG / PNG / WebP / PDF first page).
#[async_trait::async_trait]
pub trait ReceiptOcrPort: Send + Sync + 'static {
    /// The engine's name shown with a reading (`paddleocr`, `disabled`).
    fn engine(&self) -> &'static str;
    async fn read(&self, bytes: &[u8], content_type: &str) -> Result<OcrText, OcrError>;
}

/// No reader configured.
pub struct DisabledOcr;

#[async_trait::async_trait]
impl ReceiptOcrPort for DisabledOcr {
    fn engine(&self) -> &'static str {
        "disabled"
    }
    async fn read(&self, _: &[u8], _: &str) -> Result<OcrText, OcrError> {
        Err(OcrError::Disabled)
    }
}

/// The PaddleOCR sidecar over HTTP.
pub struct PaddleOcrHttp {
    base: String,
    client: reqwest::Client,
}

impl PaddleOcrHttp {
    /// `base` like `http://ocr:8866`; requests time out after `timeout`.
    pub fn new(base: &str, timeout: Duration) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .unwrap_or_default(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OcrRequest<'a> {
    content_type: &'a str,
    data_base64: String,
}

#[async_trait::async_trait]
impl ReceiptOcrPort for PaddleOcrHttp {
    fn engine(&self) -> &'static str {
        "paddleocr"
    }
    async fn read(&self, bytes: &[u8], content_type: &str) -> Result<OcrText, OcrError> {
        let body = OcrRequest {
            content_type,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        };
        let res = self
            .client
            .post(format!("{}/ocr", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| OcrError::Unavailable(e.without_url().to_string()))?;
        if !res.status().is_success() {
            return Err(OcrError::Unavailable(format!(
                "status {}",
                res.status().as_u16()
            )));
        }
        let mut text: OcrText = res
            .json()
            .await
            .map_err(|e| OcrError::Unavailable(e.without_url().to_string()))?;
        text.lines.truncate(400);
        for l in &mut text.lines {
            l.text = l.text.chars().take(300).collect();
        }
        if text.engine.is_empty() {
            text.engine = "paddleocr".into();
        }
        Ok(text)
    }
}

/// The reader the composition root hands Domain J: PaddleOCR at `url` (`SUTRA_OCR_URL`, read by
/// the API; requests time out after `timeout_secs`, `SUTRA_OCR_TIMEOUT_SECS`, default 30),
/// otherwise [`DisabledOcr`].
pub fn reader(url: Option<&str>, timeout_secs: Option<u64>) -> Arc<dyn ReceiptOcrPort> {
    match url.map(str::trim).filter(|v| !v.is_empty()) {
        Some(url) => {
            let secs = timeout_secs.filter(|s| (1..=300).contains(s)).unwrap_or(30);
            Arc::new(PaddleOcrHttp::new(url, Duration::from_secs(secs)))
        }
        None => Arc::new(DisabledOcr),
    }
}
