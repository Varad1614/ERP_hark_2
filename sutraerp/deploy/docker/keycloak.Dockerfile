# syntax=docker/dockerfile:1.7
# Keycloak with the SutraERP realm baked in (audit 2026-10, A1).
#
# The compose stacks used to bind-mount the realm JSON read-only into /opt/keycloak/data/import.
# Keycloak runs as UID 1000; on a host where the checkout is not world-readable (a clone made by
# root with umask 077, as on the InstaBox demo machine) it could not read the file, the realm
# import failed and the container crash-looped. Copying the file into the image with an explicit
# owner and mode removes the dependency on host file permissions.
#
#   REALM_FILE  realm export to import at start (`--import-realm`; an existing realm of the same
#               name is left untouched). Values written as ${ENV_VAR} / ${ENV_VAR:default} are
#               resolved from the container environment at import.
FROM quay.io/keycloak/keycloak:26.7@sha256:37dbaf6f0722c9ec246335f36e1ef8b2e6cb960f7c27e0d8c615121a3d475a85
ARG REALM_FILE=deploy/keycloak/realm-sutra-dev.json
# The import directory is created explicitly (0755, owned by keycloak): BuildKit applies
# `--chmod` to parent directories that COPY creates, so on newer builders (Docker 29 / buildx
# 0.37 on the demo box) /opt/keycloak/data/import came out 0644 — not traversable by UID 1000 —
# and Keycloak silently imported nothing ("Realm does not exist").
USER root
RUN mkdir -p /opt/keycloak/data/import \
 && chown 1000:0 /opt/keycloak/data/import \
 && chmod 0755 /opt/keycloak/data/import
COPY --chown=1000:0 --chmod=0644 ${REALM_FILE} /opt/keycloak/data/import/realm.json
USER 1000
