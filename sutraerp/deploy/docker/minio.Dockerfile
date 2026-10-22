# Part 2 · S4-docs: MinIO for local compose, built from the archived upstream source
# (github.com/minio/minio, AGPL-3.0). Upstream no longer publishes pullable images, and SutraERP never
# uses third-party mirror images: this builds the pinned release tag with the official Go image and
# runs it on the official Debian slim image as a non-root user.
#   docker compose build minio      (≈ 3–6 min the first time; needs the Go module proxy)
# Production: any S3-compatible service the college trusts (AWS S3, a self-built MinIO, SeaweedFS,
# Garage, Ceph RGW …) — see .env.example "Document storage".
ARG MINIO_TAG=RELEASE.2025-10-15T17-29-55Z
FROM golang:1.24-trixie@sha256:5835f052b784aa39f2fe9070def3568605c8bc3fcd810f10402066348b61e716 AS build
ARG MINIO_TAG
COPY deploy/minio/build-from-source.sh /build-from-source.sh
RUN MINIO_TAG="$MINIO_TAG" OUT=/out/minio bash /build-from-source.sh

FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
ARG MINIO_TAG
LABEL org.opencontainers.image.title="sutraerp-minio (built from source ${MINIO_TAG})" \
      org.opencontainers.image.source="https://github.com/minio/minio" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
COPY --from=build /out/minio /usr/local/bin/minio
COPY deploy/minio/healthcheck.sh /usr/local/bin/minio-healthcheck
RUN useradd --system --uid 10001 --home-dir /data minio && mkdir -p /data && chown minio /data
ENV MINIO_BROWSER=off MINIO_UPDATE=off
USER minio
EXPOSE 9000
VOLUME ["/data"]
HEALTHCHECK --interval=5s --timeout=3s --retries=20 CMD ["/usr/local/bin/minio-healthcheck"]
ENTRYPOINT ["/usr/local/bin/minio"]
CMD ["server", "--address", ":9000", "/data"]
