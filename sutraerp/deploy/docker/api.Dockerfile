# syntax=docker/dockerfile:1.7
# ---- build ----
# Workspace layout: crates/{kernel,contracts,persistence,web,infra}, modules/* (10 domains incl. test
# fixtures under modules/platform/tests), apps/api; migrations/ are embedded by sqlx::migrate! and
# catalog/ by include_str!. apps/web is not a Cargo member. webauthn-rs links the system OpenSSL:
# rust:1-bookworm ships pkg-config + libssl-dev, and the distroless cc runtime ships libssl3.
FROM rust:1-bookworm AS build
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
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /sutra-api /usr/local/bin/sutra-api
ENV SUTRA_ENV=production SUTRA_HTTP_ADDR=0.0.0.0:8080 SUTRA_LOG_FORMAT=json
# `serve` (default) runs as the runtime DB role in DATABASE_URL; `migrate` runs as the schema owner
# (SUTRA_MIGRATION_DATABASE_URL, else DATABASE_URL) and provisions SUTRA_DB_RUNTIME_USER.
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/sutra-api"]
CMD ["serve"]
