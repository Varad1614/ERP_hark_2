# Part 2 · S4-docs: the runtime stage of minio.Dockerfile around a MinIO binary built on the host
# from source (deploy/minio/build-from-source.sh, copied to deploy/docker/demo-bin/minio by the demo
# workflow, which caches it per release tag). Used only by docker-compose.demo.yml.
FROM debian:13-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
LABEL org.opencontainers.image.title="sutraerp-minio-demo (built from source)" \
      org.opencontainers.image.source="https://github.com/minio/minio" \
      org.opencontainers.image.licenses="AGPL-3.0-only"
COPY deploy/docker/demo-bin/minio /usr/local/bin/minio
COPY deploy/minio/healthcheck.sh /usr/local/bin/minio-healthcheck
RUN useradd --system --uid 10001 --home-dir /data minio && mkdir -p /data && chown minio /data
ENV MINIO_BROWSER=off MINIO_UPDATE=off
USER minio
EXPOSE 9000
VOLUME ["/data"]
HEALTHCHECK --interval=5s --timeout=3s --retries=20 CMD ["/usr/local/bin/minio-healthcheck"]
ENTRYPOINT ["/usr/local/bin/minio"]
CMD ["server", "--address", ":9000", "/data"]
