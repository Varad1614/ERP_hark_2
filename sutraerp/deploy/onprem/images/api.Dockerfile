# On-prem release image of the API (Part 2 · S5-deploy): the runtime stage of
# deploy/docker/api.Dockerfile around a sutra-api binary built on the CI runner with
# `cargo build --profile docker -p sutra-api --locked` (same profile as the kind images), copied to
# deploy/onprem/images/bin/sutra-api by the on-prem workflow. Distroless, non-root, no shell.
# The CA bundle the operator mounts at /etc/sutraerp/tls/trust.pem is trusted for outbound HTTPS
# through SSL_CERT_FILE (rustls-tls-native-roots, releases from 2026.10.1).
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf
ARG SUTRA_GIT_SHA=unknown
ARG SUTRA_VERSION=unknown
LABEL org.opencontainers.image.source="https://github.com/Varad1614/ERP_hark_2" \
      org.opencontainers.image.title="sutraerp-api" \
      org.opencontainers.image.version="${SUTRA_VERSION}" \
      org.opencontainers.image.revision="${SUTRA_GIT_SHA}"
COPY bin/sutra-api /usr/local/bin/sutra-api
ENV SUTRA_ENV=production SUTRA_HTTP_ADDR=0.0.0.0:8080 SUTRA_LOG_FORMAT=json SUTRA_GIT_SHA=${SUTRA_GIT_SHA}
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/sutra-api"]
CMD ["serve"]
