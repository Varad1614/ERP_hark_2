# SutraERP receipt OCR sidecar: PaddleOCR (Apache-2.0) PP-OCRv6 small models behind a tiny HTTP API.
#   docker build -t sutraerp-ocr tools/ocr-sidecar
#   docker run --rm -p 8866:8866 sutraerp-ocr
FROM python:3.11-slim@sha256:cce12316475d7d56d605f76e1d811d0dc4e7227f2d4afd29951521264f504ff8

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    OCR_LANG=en \
    OCR_PORT=8866 \
    HOME=/home/ocr \
    PADDLE_PDX_DISABLE_MODEL_SOURCE_CHECK=True
# (Models are baked into the image at build time, so start-up does not probe model hosts.)

# Runtime libraries Paddle / OpenCV need on slim images.
RUN apt-get update \
 && apt-get install -y --no-install-recommends libgomp1 libglib2.0-0 libgl1 \
 && rm -rf /var/lib/apt/lists/*

RUN groupadd --system --gid 10001 ocr \
 && useradd --system --uid 10001 --gid ocr --home-dir /home/ocr --create-home ocr

WORKDIR /app
COPY requirements.txt .
RUN pip install -r requirements.txt

COPY app.py .
USER ocr
# Pre-download the detection + recognition models into ~/.paddlex so the container starts offline.
ARG OCR_LANG=en
RUN OCR_LANG=${OCR_LANG} python -c "import logging, app; logging.basicConfig(level='INFO'); app.build_engine()"

EXPOSE 8866
HEALTHCHECK --interval=15s --timeout=5s --start-period=60s --retries=5 \
  CMD python -c "import urllib.request,sys; sys.exit(0 if urllib.request.urlopen('http://127.0.0.1:8866/healthz', timeout=4).status == 200 else 1)"

CMD ["python", "app.py"]
