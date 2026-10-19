# syntax=docker/dockerfile:1.7
# ---- build (static SPA artifact, ADR-L5-0015) ----
FROM node:22-bookworm-slim AS build
WORKDIR /src
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml .npmrc tsconfig.base.json ./
COPY apps/web/package.json apps/web/
COPY packages/ui/package.json packages/ui/
COPY packages/api-client/package.json packages/api-client/
RUN pnpm install --frozen-lockfile
COPY catalog catalog
COPY openapi openapi
COPY packages packages
COPY apps/web apps/web
# Audit 2026-10 (A1 · C1/M10): images default to real OIDC sign-in. `api` (dev authenticator,
# login id + typed password) is for development stacks only; VITE_DEV_PERSONAS=true adds the dev
# persona picker / role switcher and must never be set for a public image.
ARG VITE_AUTH_MODE=oidc
ARG VITE_DEV_PERSONAS=false
ENV VITE_AUTH_MODE=$VITE_AUTH_MODE VITE_DEV_PERSONAS=$VITE_DEV_PERSONAS
RUN pnpm --filter @sutra/web build
# Part 1 · Stage 3 (S3-config): SHA-256 of the SPA's inline bootstrap scripts → nginx CSP snippet
# (replaces script-src 'unsafe-inline'); fails the build on inline event handlers.
COPY deploy/docker/csp-hashes.mjs deploy/docker/csp-hashes.mjs
RUN node deploy/docker/csp-hashes.mjs apps/web/build/client /src/sutra-csp.conf

# ---- runtime: unprivileged nginx, SPA fallback + /api reverse proxy ----
# 9D: security (SEC-9D-013) — 1.27-alpine stopped receiving rebuilds in 2025-06 (stale
# Alpine packages); 1.30 is the maintained nginx stable branch.
FROM nginxinc/nginx-unprivileged:1.30-alpine
# 9D: security (SEC-9D-016) — pick up Alpine security fixes released after the base image was
# built (zlib 1.3.2-r1, found by Grype); the base runs as UID 101, so upgrade as root and drop back.
USER root
# P4: CVE-2026-77214 (libexpat < 2.9.0-r0, Trivy HIGH). The explicit floor also changes this
# layer's cache key, so a cached `apk upgrade` layer from before the fix cannot be reused.
RUN apk upgrade --no-cache && apk add --no-cache 'libexpat>=2.9.0-r0'
USER 101
# 9B: OCI metadata (docs/batch9-contracts.md §10.2).
ARG SUTRA_GIT_SHA=unknown
LABEL org.opencontainers.image.source="https://github.com/Varad1614/ERP_hark_2" \
      org.opencontainers.image.title="sutraerp-web" \
      org.opencontainers.image.revision="${SUTRA_GIT_SHA}"
COPY deploy/docker/nginx.conf /etc/nginx/conf.d/default.conf
# Audit 2026-10 (A1): SUTRA_WEB_KEYCLOAK_PROXY=1 = also proxy the browser-facing Keycloak realm
# endpoints under /auth (public OIDC demo, docker-compose.oidc-demo.yml). Off by default.
ARG SUTRA_WEB_KEYCLOAK_PROXY=0
COPY deploy/docker/nginx-keycloak.conf /tmp/nginx-keycloak.conf
USER root
RUN mkdir -p /etc/nginx/sutra-extra \
 && if [ "$SUTRA_WEB_KEYCLOAK_PROXY" = "1" ]; then cp /tmp/nginx-keycloak.conf /etc/nginx/sutra-extra/keycloak.conf; fi \
 && rm /tmp/nginx-keycloak.conf && chmod -R a+rX /etc/nginx/sutra-extra
USER 101
COPY --from=build /src/sutra-csp.conf /etc/nginx/sutra-csp.conf
COPY --from=build /src/apps/web/build/client /usr/share/nginx/html
EXPOSE 8080
