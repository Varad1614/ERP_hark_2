#!/usr/bin/env bash
# Part 2 · S4-docs: runs a MinIO binary (deploy/minio/build-from-source.sh) as a background host
# process for CI / local tests, waits until it is live, creates the bucket(s) with a SigV4-signed PUT
# (curl --aws-sigv4) and proves anonymous listing of them is refused.
#
#   MINIO_BIN=./minio MINIO_ROOT_USER=… MINIO_ROOT_PASSWORD=… MINIO_BUCKETS="a b" bash deploy/minio/start-local.sh
#
# Data in MINIO_DATA (default a temp dir), log in MINIO_LOG, listens on MINIO_ADDR (127.0.0.1:9000).
set -euo pipefail
: "${MINIO_BIN:?}" "${MINIO_ROOT_USER:?}" "${MINIO_ROOT_PASSWORD:?}"
MINIO_ADDR="${MINIO_ADDR:-127.0.0.1:9000}"
MINIO_DATA="${MINIO_DATA:-$(mktemp -d)}"
MINIO_LOG="${MINIO_LOG:-$MINIO_DATA.log}"
REGION="${MINIO_REGION:-us-east-1}"
export MINIO_ROOT_USER MINIO_ROOT_PASSWORD MINIO_BROWSER=off MINIO_UPDATE=off MINIO_REGION="$REGION"
nohup "$MINIO_BIN" server --quiet --address "$MINIO_ADDR" "$MINIO_DATA" >"$MINIO_LOG" 2>&1 &
echo "minio pid $! on $MINIO_ADDR (data $MINIO_DATA, log $MINIO_LOG)"
for _ in $(seq 1 60); do
  curl -fsS -o /dev/null -m 2 "http://$MINIO_ADDR/minio/health/live" && break
  sleep 1
done
curl -fsS -o /dev/null -m 2 "http://$MINIO_ADDR/minio/health/live" || { echo "minio did not become live" >&2; cat "$MINIO_LOG" >&2; exit 1; }
for b in ${MINIO_BUCKETS:-}; do
  code=$(curl -sS -o /dev/null -w '%{http_code}' -X PUT --aws-sigv4 "aws:amz:$REGION:s3" \
    --user "$MINIO_ROOT_USER:$MINIO_ROOT_PASSWORD" "http://$MINIO_ADDR/$b")
  case "$code" in 200|409) echo "bucket $b ready (HTTP $code)" ;; *) echo "creating bucket $b: HTTP $code" >&2; exit 1 ;; esac
  anon=$(curl -sS -o /dev/null -w '%{http_code}' "http://$MINIO_ADDR/$b?list-type=2")
  [ "$anon" = 403 ] || { echo "anonymous listing of $b answered $anon (expected 403)" >&2; exit 1; }
done
"$MINIO_BIN" --version
