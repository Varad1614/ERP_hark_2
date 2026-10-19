#!/usr/bin/env bash
# Part 2 · S5-capacity: throw-away secrets + a private CA for the capacity stack
# (deploy/capacity/README.md). Writes, under $CAP_DIR (default ./capacity-run):
#   env            compose environment file (random passwords / keys, never committed)
#   certs/ca.pem   the private CA (trusted by the host, the API via SUTRA_TLS_CA_FILE, node, k6)
#   certs/edge.{crt,key}  TLS for erp/sso/s3.<domain> (edge nginx)
#   certs/pg.{crt,key}    PostgreSQL server TLS (sslmode=require)
# On a college server use the institution's own certificates and secrets store instead
# (CA_PEM / EDGE_CRT / EDGE_KEY can point at them; see the report's "To complete" section).
set -euo pipefail
CAP_DIR="${CAP_DIR:-$PWD/capacity-run}"
DOMAIN="${CAP_DOMAIN:-sutra.test}"
mkdir -p "$CAP_DIR/certs"
cd "$CAP_DIR/certs"
rnd() { openssl rand -hex "${1:-24}"; }

if [[ ! -f ca.pem ]]; then
  openssl req -x509 -newkey rsa:3072 -nodes -days 30 -subj "/CN=SutraERP capacity test CA" \
    -keyout ca.key -out ca.pem >/dev/null 2>&1
fi
issue() { # name, SAN list
  openssl req -newkey rsa:2048 -nodes -subj "/CN=$1" -keyout "$1.key" -out "$1.csr" >/dev/null 2>&1
  printf 'subjectAltName=%s\nextendedKeyUsage=serverAuth\nkeyUsage=digitalSignature,keyEncipherment\n' "$2" >"$1.ext"
  openssl x509 -req -in "$1.csr" -CA ca.pem -CAkey ca.key -CAcreateserial -days 30 -sha256 \
    -extfile "$1.ext" -out "$1.crt" >/dev/null 2>&1
  rm -f "$1.csr" "$1.ext"
}
[[ -f edge.crt ]] || issue edge "DNS:erp.$DOMAIN,DNS:sso.$DOMAIN,DNS:s3.$DOMAIN"
[[ -f pg.crt ]] || issue pg "DNS:postgres,DNS:localhost,IP:127.0.0.1"
chmod 644 ca.pem edge.crt pg.crt edge.key
# PostgreSQL refuses a key readable by others; the postgres image runs as uid 999.
chmod 600 pg.key
if [[ "$(id -u)" == 0 ]]; then chown 999:999 pg.key; else sudo chown 999:999 pg.key 2>/dev/null || true; fi

cd "$CAP_DIR"
if [[ ! -f env ]]; then
  {
    echo "CAP_DOMAIN=$DOMAIN"
    echo "POSTGRES_PASSWORD=$(rnd)"
    echo "SUTRA_RUNTIME_DB_PASSWORD=$(rnd)"
    echo "KEYCLOAK_DB_PASSWORD=$(rnd)"
    echo "FLOWABLE_DB_PASSWORD=$(rnd)"
    echo "FLOWABLE_REST_PASSWORD=$(rnd)"
    echo "KC_ADMIN_PASSWORD=$(rnd)"
    echo "OIDC_CLIENT_SECRET=$(rnd)"
    echo "IDP_CLIENT_SECRET=$(rnd)"
    echo "API_CLIENT_SECRET=$(rnd)"
    echo "S3_ACCESS_KEY=cap$(rnd 6)"
    echo "S3_SECRET_KEY=$(rnd)"
    echo "MINIO_KMS_SECRET_KEY=sutra-capacity-key:$(openssl rand -base64 32)"
    echo "SUTRA_CSRF_KEY=$(rnd 32)"
    echo "SUTRA_MFA_ENCRYPTION_KEY=$(rnd 32)"
    echo "SUTRA_AUDIT_ANCHOR_KEY=$(rnd 32)"
    # Every seeded / synthetic account signs in with this password (Keycloak keeps its own
    # pbkdf2 hash; the value is generated per run and masked in CI logs).
    echo "SEED_PASSWORD=Cap-$(rnd 12)"
  } >env
  chmod 600 env
fi
echo "ok capacity secrets + CA in $CAP_DIR"
