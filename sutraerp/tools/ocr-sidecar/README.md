# Receipt OCR sidecar (PaddleOCR)

A small HTTP service that reads the text of staff expense receipts for SutraERP slice **3D
(expense claims)**. The API calls it when a claimant uploads a receipt and uses the lines to
pre-fill the draft expense (merchant, date, total, GST, GSTIN, payment mode) —
`modules/resources/src/expenses/ocr.rs` (client) and `receipt_text.rs` (field heuristics).

* **Engine:** [PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) 3.x (Apache-2.0) with the
  PP-OCRv6 *small* detection + recognition models (CPU). It runs as a separate service and is not
  linked into SutraERP; see `THIRD_PARTY_NOTICES.md`.
* **Privacy:** self-hosted. Receipts are decoded in memory, never written to disk, never logged
  and never sent to a third party — they do not leave the institution's servers.
* **A person decides:** OCR only pre-fills fields. The claimant confirms every value before an
  expense counts, and approvers see the receipt itself.

## Run

With the rest of the stack (compose profile `ocr`, see `docker-compose.yml`):

```sh
docker compose --profile app --profile ocr up -d --build
```

Then give the API the sidecar's address (compose service name `ocr`):

| Variable (API) | Example | Meaning |
|---|---|---|
| `SUTRA_OCR_URL` | `http://ocr:8866` (compose) / `http://localhost:8866` (`make api`) | Base URL of this sidecar. Unset = receipt reading disabled: uploads still work, nothing is pre-filled. |
| `SUTRA_OCR_TIMEOUT_SECS` | `30` | Per-request timeout, 1–300 s (default 30). |

Standalone:

```sh
docker build -t sutraerp-ocr tools/ocr-sidecar
docker run --rm -p 8866:8866 sutraerp-ocr
curl -s localhost:8866/healthz      # {"ok":true} once the model is loaded
```

The image downloads the models at build time (≈ 30 MB), so the container starts without
internet access. Expect ~1 GB RAM and 0.3–2 s per receipt on a modern CPU.

### Sidecar settings

| Variable | Default | Meaning |
|---|---|---|
| `OCR_LANG` | `en` | Recognition language. `en`, `ch`, `japan` and Latin-script languages use the PP-OCRv6 small pair; others (e.g. `devanagari`, `ta`, `te`) fall back to PaddleOCR's PP-OCRv5 models for that language. Also a build arg, so the right models are baked in. |
| `OCR_DET_MODEL` / `OCR_REC_MODEL` | — | Override the model names (e.g. `PP-OCRv6_tiny_det` / `PP-OCRv6_tiny_rec` on very small hosts). Bake them in by rebuilding. |
| `OCR_DEVICE` | `cpu` | Paddle device string. |
| `OCR_PORT` | `8866` | Listen port. |

## Contract

`POST /ocr`

```json
{ "contentType": "image/jpeg", "dataBase64": "<base64 of the file>" }
```

`contentType` is one of `image/jpeg`, `image/png`, `image/webp`, `application/pdf` (first page
only, rendered at ~200 dpi). The request body is capped at 3 MB (the API sends at most 1.5 MB
files); images are converted to RGB and their long edge capped at 2000 px.

`200`

```json
{
  "lines": [
    { "text": "CAFE MADRAS", "confidence": 0.98 },
    { "text": "Grand Total ₹ 378.00", "confidence": 0.95 }
  ],
  "engine": "paddleocr"
}
```

Lines are ordered top to bottom, then left to right. Text boxes are grouped into printed rows by
their vertical centre and joined with spaces, so a label and its amount on one printed row arrive
as one line (the API parses totals per line). `confidence` is 0…1, character-weighted over the
row. Errors are non-2xx with `{"error": "..."}`: `400` bad JSON, `413` too large, `422`
unsupported type / bad base64 / unreadable file, `500` OCR failure, `503` model still loading.

`GET /healthz` → `200 {"ok": true}` (503 until the model is loaded).

## Tests

The row grouping and other pure helpers are tested without Paddle installed:

```sh
cd tools/ocr-sidecar && python3 -m unittest test_rows.py
```
