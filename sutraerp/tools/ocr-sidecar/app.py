"""SutraERP receipt OCR sidecar (PaddleOCR, Apache-2.0).

Serves the contract the API's `PaddleOcrHttp` client speaks
(`modules/resources/src/expenses/ocr.rs`):

    POST /ocr      {"contentType": "image/jpeg" | "image/png" | "image/webp" | "application/pdf",
                    "dataBase64": "..."}
                -> 200 {"lines": [{"text": "...", "confidence": 0.97}, ...], "engine": "paddleocr"}
    GET  /healthz  -> 200 {"ok": true}   (503 until the model is loaded)

Lines come out top to bottom, then left to right: recognised boxes are grouped into rows by their
vertical centre and each row is joined with spaces, so "Grand Total", "₹", "378.00" printed on one
line arrive as the single line "Grand Total ₹ 378.00" (the API parses totals per line).

Receipts are processed in memory and never written to disk or sent anywhere else. OCR only
pre-fills a draft expense; a person confirms every field.

Paddle is imported inside `build_engine()` only, so the pure helpers (`group_rows`,
`boxes_from_result`, `fit_long_edge`) import and test without paddle installed.
"""

from __future__ import annotations

import base64
import binascii
import io
import logging
import os
import threading
from contextlib import asynccontextmanager
from typing import Any, Iterable, Sequence

ENGINE_NAME = "paddleocr"
MAX_BODY_BYTES = 3 * 1024 * 1024  # whole JSON request
MAX_DECODED_BYTES = 3 * 1024 * 1024  # the file inside it (the API sends at most 1.5 MB)
MAX_LONG_EDGE = 2000  # px; larger images are downscaled before OCR
PDF_DPI = 200
MAX_LINES = 400  # the API truncates to 400 as well
ALLOWED_TYPES = ("image/jpeg", "image/png", "image/webp", "application/pdf")

# Languages the single PP-OCRv6 recognition model covers (Chinese, English, Japanese and the
# Latin-script languages). Anything else falls back to PaddleOCR's per-language PP-OCRv5 models.
PPOCRV6_LANGS = {
    "en", "ch", "japan", "latin", "fr", "german", "de", "es", "it", "pt", "nl", "id", "ms", "vi",
}
DEFAULT_DET_MODEL = "PP-OCRv6_small_det"
DEFAULT_REC_MODEL = "PP-OCRv6_small_rec"

log = logging.getLogger("sutra-ocr")


# --------------------------------------------------------------------------- pure helpers


def _bbox(points: Any) -> tuple[float, float, float, float] | None:
    """(x0, y0, x1, y1) from a 4-point polygon, an [x0, y0, x1, y1] box, or a numpy array."""
    try:
        pts = points.tolist() if hasattr(points, "tolist") else list(points)
    except TypeError:
        return None
    if not pts:
        return None
    if all(isinstance(p, (int, float)) for p in pts):
        if len(pts) != 4:
            return None
        x0, y0, x1, y1 = (float(v) for v in pts)
        return (min(x0, x1), min(y0, y1), max(x0, x1), max(y0, y1))
    try:
        xs = [float(p[0]) for p in pts]
        ys = [float(p[1]) for p in pts]
    except (TypeError, IndexError, ValueError):
        return None
    return (min(xs), min(ys), max(xs), max(ys))


def boxes_from_result(result: Any) -> list[dict]:
    """Flatten one PaddleOCR 3.x `predict` result (dict-like with `rec_texts`, `rec_scores`,
    `rec_polys` or `rec_boxes`) into boxes `{text, confidence, x0, y0, x1, y1}`."""
    def get(key: str) -> Any:
        try:
            v = result[key]
        except (KeyError, TypeError, IndexError):
            v = None
        return [] if v is None else v

    texts = list(get("rec_texts"))
    scores = list(get("rec_scores"))
    polys = get("rec_polys")
    if len(polys) == 0:
        polys = get("rec_boxes")
    polys = list(polys)
    out = []
    for i, text in enumerate(texts):
        text = str(text).strip()
        if not text or i >= len(polys):
            continue
        bb = _bbox(polys[i])
        if bb is None:
            continue
        conf = float(scores[i]) if i < len(scores) else 0.0
        out.append({"text": text, "confidence": conf, "x0": bb[0], "y0": bb[1], "x1": bb[2], "y1": bb[3]})
    return out


def group_rows(boxes: Iterable[dict], tolerance: float = 0.5) -> list[dict]:
    """Group text boxes into printed rows and return lines top to bottom.

    A box joins the current row when its vertical centre lies within `tolerance` × the row's
    median box height of the row's mean centre. Within a row boxes are ordered left to right and
    joined with single spaces; the row's confidence is the character-weighted mean of its boxes.
    Returns `[{"text": str, "confidence": float}]` with confidence rounded to 4 places.
    """
    items = []
    for b in boxes:
        text = str(b.get("text", "")).strip()
        if not text:
            continue
        y0, y1 = float(b["y0"]), float(b["y1"])
        items.append({
            "text": text,
            "confidence": max(0.0, min(1.0, float(b.get("confidence", 0.0)))),
            "x0": float(b["x0"]),
            "yc": (y0 + y1) / 2.0,
            "h": max(1.0, y1 - y0),
        })
    items.sort(key=lambda it: (it["yc"], it["x0"]))

    rows: list[list[dict]] = []
    for it in items:
        if rows:
            row = rows[-1]
            centre = sum(r["yc"] for r in row) / len(row)
            heights = sorted(r["h"] for r in row)
            h = heights[len(heights) // 2]
            if abs(it["yc"] - centre) <= tolerance * max(h, it["h"] * 0.5):
                row.append(it)
                continue
        rows.append([it])

    lines = []
    for row in rows:
        row.sort(key=lambda r: r["x0"])
        text = " ".join(" ".join(r["text"].split()) for r in row)
        weight = sum(len(r["text"]) for r in row) or 1
        conf = sum(r["confidence"] * len(r["text"]) for r in row) / weight
        lines.append({"text": text, "confidence": round(conf, 4)})
    return lines


def fit_long_edge(width: int, height: int, limit: int = MAX_LONG_EDGE) -> tuple[int, int]:
    """Size that caps the longer edge at `limit`, keeping the aspect ratio (never upscales)."""
    long_edge = max(width, height)
    if long_edge <= limit or long_edge <= 0:
        return (width, height)
    scale = limit / float(long_edge)
    return (max(1, round(width * scale)), max(1, round(height * scale)))


def decode_payload(body: Any) -> tuple[str, bytes]:
    """Validate the request JSON and return (contentType, file bytes); raises ValueError."""
    if not isinstance(body, dict):
        raise ValueError("body must be a JSON object")
    content_type = str(body.get("contentType", "")).split(";")[0].strip().lower()
    if content_type == "image/jpg":
        content_type = "image/jpeg"
    if content_type not in ALLOWED_TYPES:
        raise ValueError("unsupported contentType")
    data = body.get("dataBase64")
    if not isinstance(data, str) or not data:
        raise ValueError("dataBase64 is required")
    try:
        raw = base64.b64decode(data, validate=True)
    except (binascii.Error, ValueError):
        raise ValueError("dataBase64 is not valid base64") from None
    if not raw:
        raise ValueError("empty file")
    if len(raw) > MAX_DECODED_BYTES:
        raise ValueError("file too large")
    return content_type, raw


# --------------------------------------------------------------------------- image loading


def load_image(content_type: str, raw: bytes):
    """RGB PIL image of the upload (first page for PDFs), long edge capped at MAX_LONG_EDGE."""
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = 60_000_000  # refuse decompression bombs
    if content_type == "application/pdf":
        import pypdfium2 as pdfium

        pdf = pdfium.PdfDocument(raw)
        try:
            if len(pdf) == 0:
                raise ValueError("PDF has no pages")
            page = pdf[0]
            w, h = page.get_size()  # PDF points (1/72 in)
            scale = PDF_DPI / 72.0
            long_px = max(w, h) * scale
            if long_px > MAX_LONG_EDGE:
                scale *= MAX_LONG_EDGE / long_px
            img = page.render(scale=scale).to_pil()
            page.close()
        finally:
            pdf.close()
    else:
        img = Image.open(io.BytesIO(raw))
        img.load()
    img = img.convert("RGB")
    size = fit_long_edge(img.width, img.height)
    if size != (img.width, img.height):
        img = img.resize(size, Image.LANCZOS)
    return img


# --------------------------------------------------------------------------- engine


def engine_options(lang: str) -> dict:
    """PaddleOCR constructor options: PP-OCRv6 small models for the languages they cover (override
    with OCR_DET_MODEL / OCR_REC_MODEL), otherwise PaddleOCR's default models for `lang`."""
    opts: dict = {
        "use_doc_orientation_classify": False,
        "use_doc_unwarping": False,
        "use_textline_orientation": False,
        "device": os.environ.get("OCR_DEVICE", "cpu"),
    }
    det = os.environ.get("OCR_DET_MODEL", "").strip()
    rec = os.environ.get("OCR_REC_MODEL", "").strip()
    if not det and not rec and lang in PPOCRV6_LANGS:
        det, rec = DEFAULT_DET_MODEL, DEFAULT_REC_MODEL
    if det:
        opts["text_detection_model_name"] = det
    if rec:
        opts["text_recognition_model_name"] = rec
    if not rec:
        opts["lang"] = lang
    return opts


def build_engine():
    """Load PaddleOCR once (also run at image build time to pre-download the models)."""
    from paddleocr import PaddleOCR  # imported here so tests run without paddle

    lang = os.environ.get("OCR_LANG", "en").strip() or "en"
    opts = engine_options(lang)
    log.info("loading PaddleOCR: %s", {k: v for k, v in opts.items()})
    return PaddleOCR(**opts)


class Reader:
    """Holds the engine; PaddleOCR predictors are not documented thread-safe, so calls serialise."""

    def __init__(self, engine: Any):
        self.engine = engine
        self.lock = threading.Lock()

    def read(self, content_type: str, raw: bytes) -> list[dict]:
        import numpy as np

        img = load_image(content_type, raw)
        bgr = np.ascontiguousarray(np.asarray(img)[:, :, ::-1])  # PaddleOCR expects BGR arrays
        with self.lock:
            results = list(self.engine.predict(bgr))
        boxes: list[dict] = []
        for res in results:
            boxes.extend(boxes_from_result(res))
        return group_rows(boxes)[:MAX_LINES]


# --------------------------------------------------------------------------- HTTP


def create_app(reader_factory=None):
    import json

    from fastapi import FastAPI, Request
    from fastapi.concurrency import run_in_threadpool
    from fastapi.responses import JSONResponse

    state: dict = {"reader": None}
    factory = reader_factory or (lambda: Reader(build_engine()))

    @asynccontextmanager
    async def lifespan(_app):
        state["reader"] = await run_in_threadpool(factory)
        log.info("receipt reader ready")
        yield

    app = FastAPI(title="SutraERP OCR sidecar", docs_url=None, redoc_url=None, openapi_url=None,
                  lifespan=lifespan)

    def error(status: int, message: str) -> JSONResponse:
        return JSONResponse(status_code=status, content={"error": message})

    @app.get("/healthz")
    async def healthz():
        if state["reader"] is None:
            return JSONResponse(status_code=503, content={"ok": False})
        return {"ok": True}

    @app.post("/ocr")
    async def ocr(request: Request):
        reader = state["reader"]
        if reader is None:
            return error(503, "model not loaded")
        length = request.headers.get("content-length")
        if length is not None:
            try:
                if int(length) > MAX_BODY_BYTES:
                    return error(413, "request too large")
            except ValueError:
                return error(400, "bad content-length")
        body = bytearray()
        async for chunk in request.stream():
            body.extend(chunk)
            if len(body) > MAX_BODY_BYTES:
                return error(413, "request too large")
        try:
            payload = json.loads(bytes(body))
        except (ValueError, UnicodeDecodeError):
            return error(400, "body must be JSON")
        try:
            content_type, raw = decode_payload(payload)
        except ValueError as e:
            status = 413 if str(e) == "file too large" else 422
            return error(status, str(e))
        try:
            lines = await run_in_threadpool(reader.read, content_type, raw)
        except ValueError as e:
            return error(422, str(e))
        except Exception:  # noqa: BLE001 - never echo receipt contents or tracebacks
            log.exception("OCR failed")
            return error(500, "could not read the receipt")
        return {"lines": lines, "engine": ENGINE_NAME}

    return app


if __name__ == "__main__":
    import uvicorn

    logging.basicConfig(level=os.environ.get("LOG_LEVEL", "INFO"))
    uvicorn.run(
        create_app(),
        host=os.environ.get("OCR_HOST", "0.0.0.0"),
        port=int(os.environ.get("OCR_PORT", "8866")),
        workers=1,
        access_log=False,  # request lines only; bodies are never logged anyway
    )
