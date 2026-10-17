# Part 2 · S4-docs: MinIO for local compose, built from the archived upstream source
# (github.com/minio/minio, AGPL-3.0). Upstream no longer publishes pullable images, and SutraERP never
# uses third-party mirror images: this builds the pinned release tag with the official Go image and
# runs it on the official Debian slim image as a non-root user.
#   docker compose build minio      (≈ 3–6 min the first time; needs the Go module proxy)
# Production: any S3-compatible service the college trusts (AWS S3, a self-built MinIO, SeaweedFS,
# Garage, Ceph RGW …) — see .env.example "Document storage".
ARG MINIO_TAG=RELEASE.2025-10-15T17-29-55Z
FROM golang:1.24-trixie AS build
ARG MINIO_TAG
COPY deploy/minio/build-from-source.sh /build-from-source.sh
RUN MINIO_TAG="$MINIO_TAG" OUT=/out/minio bash /build-from-source.sh

FROM debian:13-slim
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
