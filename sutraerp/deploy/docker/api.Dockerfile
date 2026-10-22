# syntax=docker/dockerfile:1.7
# ---- build ----
# Workspace layout: crates/{kernel,contracts,persistence,web,infra}, modules/* (10 domains incl. test
# fixtures under modules/platform/tests), apps/api; migrations/ are embedded by sqlx::migrate! and
# catalog/ by include_str!. apps/web is not a Cargo member. webauthn-rs links the system OpenSSL:
# rust:1-bookworm ships pkg-config + libssl-dev, and the distroless cc runtime ships libssl3.
# 9D: security (SEC-9D-015) — Debian 13 build + runtime: distroless cc-debian12 still ships
# libssl3 3.0.20-1~deb12u2 (1 Critical + 3 High OpenSSL CVEs fixed in bookworm-security
# 3.0.22-1~deb12u1, found by Grype); cc-debian13 tracks the fixed trixie OpenSSL.
FROM rust:1.99.1-trixie@sha256:a5ab4292e80567dc92f642e2a7d63773ebd695b82b56e8f06dc8d6ecc5600b76 AS build
WORKDIR /src
# CARGO_PROFILE=release for production images; the default `docker` profile keeps local builds
# within Docker Desktop memory. CARGO_BUILD_JOBS caps parallel rustc (set 1 on 8 GB machines).
ARG CARGO_PROFILE=docker
ARG CARGO_BUILD_JOBS=2
ENV SQLX_OFFLINE=true CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .cargo .cargo
COPY crates crates
COPY modules modules
COPY apps/api apps/api
COPY migrations migrations
COPY catalog catalog
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --profile ${CARGO_PROFILE} --locked -p sutra-api && cp target/${CARGO_PROFILE}/sutra-api /sutra-api

# ---- runtime ----
# 9D: security (SEC-9D-015)
FROM gcr.io/distroless/cc-debian13:nonroot@sha256:83b9b50586253821cf63ffcec87d3d94265a093ad30da232039af962dd8f4edf
# 9B: git sha of the build (sutra_build_info{git_sha}, docs/batch9-contracts.md §3.3 / §10.2).
ARG SUTRA_GIT_SHA=unknown
LABEL org.opencontainers.image.source="https://github.com/Varad1614/ERP_hark_2" \
      org.opencontainers.image.title="sutraerp-api" \
      org.opencontainers.image.revision="${SUTRA_GIT_SHA}"
COPY --from=build /sutra-api /usr/local/bin/sutra-api
ENV SUTRA_ENV=production SUTRA_HTTP_ADDR=0.0.0.0:8080 SUTRA_LOG_FORMAT=json SUTRA_GIT_SHA=${SUTRA_GIT_SHA}
# `serve` (default) runs as the runtime DB role in DATABASE_URL; `migrate` runs as the schema owner
# (SUTRA_MIGRATION_DATABASE_URL, else DATABASE_URL) and provisions SUTRA_DB_RUNTIME_USER.
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/sutra-api"]
CMD ["serve"]
