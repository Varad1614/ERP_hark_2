#!/usr/bin/env bash
# SutraERP Part 2 backup + restore drill (Part 2 · Stage 5) — the steps of
# .github/workflows/sutraerp-part2-backup.yml. Runs from sutraerp/ as root (sudo -E env PATH=…):
# the WAL archive and base backups are written by the postgres container (uid 999, mode 0600).
#
#   drill.sh prepare | base | ship-start | quiesce | fingerprint <name> | nightly | tenant-b
#            heartbeat-start | heartbeat-stop | disaster | restore | rpo | pitr | logical
#            corrupt | tenant-restore | units | report
#
# Layout under $D (RUNNER_TEMP/drill):
#   host/        "the college server": wal-archive/ (archive_command target), staging/ (base
#                backups), backups/ (local backup disk) — ALL destroyed by `disaster`
#   offsite/     the offsite copy (encrypted sets + encrypted WAL) — survives
#   keys/        the age identity, held by the operator OFF the server — survives
#   restore/     the restore work area on the rebuilt server
#   evidence/    fingerprints, logs, metrics.jsonl (one JSON line per timed step), summary.md
# Env: D, COMPOSE_FILE / COMPOSE_PROFILES (compose stack incl. deploy/backup/compose.backup.yml),
#      SUTRA_WAL_ARCHIVE_DIR, SUTRA_PG_STAGING_DIR, SUTRA_COMPOSE_S3_ACCESS_KEY / _SECRET_KEY,
#      E2E_STATE, E2E_BASE, DATABASE_URL, SUTRA_GIT_SHA.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
B=$(cd "$here/.." && pwd)            # deploy/backup
root=$(cd "$B/../.." && pwd)          # sutraerp/
cd "$root"
: "${D:?D (drill root)}"
EV=$D/evidence
M=$EV/metrics.jsonl
ENVF=$D/backup.env
PGURL=postgres://sutra:sutra@localhost:5432/sutra
KCURL=postgres://sutra:sutra@localhost:5432/keycloak
export SUTRA_BACKUP_ENV_FILE=$ENVF SUTRA_BACKUP_METRICS=$M
mkdir -p "$EV"

log() { printf '%s [drill] %s\n' "$(date -u +%H:%M:%S.%3NZ)" "$*" >&2; }
die() { log "FAIL: $*"; exit 1; }
ok() { printf 'ok %s\n' "$*"; }
now() { date +%s.%N; }
iso() { date -u +%FT%T.%3NZ; }
# timed <phase> <step> <cmd…>: runs cmd, appends {"phase","step","seconds","start","end"} to $M
timed() {
  local phase=$1 step=$2 t0 t1 s0 rc=0
  shift 2
  t0=$(now); s0=$(iso)
  "$@" || rc=$?
  t1=$(now)
  printf '{"phase":"%s","step":"%s","seconds":%s,"start":"%s","end":"%s","rc":%d}\n' "$phase" "$step" \
    "$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.1f", b - a }')" "$s0" "$(iso)" "$rc" >>"$M"
  [[ $rc -eq 0 ]] || die "$phase / $step failed (exit $rc)"
  log "$phase / $step: $(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.1f", b - a }') s"
}
pgc() { docker compose ps -q postgres; }
fixown() { if [[ -n "${SUDO_UID:-}" ]]; then chown -R "$SUDO_UID:$SUDO_GID" "$EV" "${E2E_STATE:-/nonexistent}" 2>/dev/null || true; chmod -R a+rX "$EV" 2>/dev/null || true; fi; }
trap fixown EXIT
expect_fail() { # <label> <expected-message-regex> <cmd…>: the command must fail with that message
  local label=$1 re=$2 out rc=0
  shift 2
  out=$("$@" 2>&1) || rc=$?
  printf '%s\n' "$out" >"$EV/negative-$label.log"
  [[ $rc -ne 0 ]] || die "$label: the damaged input was ACCEPTED (exit 0)"
  grep -Eq "$re" <<<"$out" || { tail -20 <<<"$out" >&2; die "$label: failed, but not with /$re/"; }
  ok "$label: detected (exit $rc) — $(grep -Em1 "$re" <<<"$out" | cut -c1-200)"
}
flip_byte() { # <file> <offset>: XOR one byte in place
  python3 - "$1" "$2" <<'PY'
import sys
p, off = sys.argv[1], int(sys.argv[2])
with open(p, "r+b") as f:
    f.seek(off); b = f.read(1); f.seek(off); f.write(bytes([b[0] ^ 0x5A]))
PY
}
latest() { ls -1d "$D/offsite/sets/$1"-* 2>/dev/null | LC_ALL=C sort | tail -1; }

cmd=${1:?command}
shift
case "$cmd" in
prepare)
  install -d -m 0755 "$D/host" "$D/offsite" "$D/restore" "$D/keys"
  install -d -o 999 -g 999 -m 0700 "$SUTRA_WAL_ARCHIVE_DIR" "$SUTRA_PG_STAGING_DIR"
  install -d -m 0700 "$D/host/backups" "$D/offsite/wal"
  [[ -f "$D/keys/identity.txt" ]] || age-keygen -o "$D/keys/identity.txt" 2>"$D/keys/recipient.txt"
  chmod 0600 "$D/keys/identity.txt"
  rcpt=$(sed -n 's/^Public key: //p' "$D/keys/recipient.txt")
  [[ "$rcpt" == age1* ]] || die "age-keygen gave no public key"
  cat >"$ENVF" <<EOF
SUTRA_BACKUP_DIR=$D/host/backups
SUTRA_ENV=drill
SUTRA_GIT_SHA=${SUTRA_GIT_SHA:-unknown}
SUTRA_BACKUP_DATABASE_URL=$PGURL
SUTRA_BACKUP_KEYCLOAK_DATABASE_URL=$KCURL
SUTRA_KC_URL=http://localhost:8180
SUTRA_KC_ADMIN_USER=admin
SUTRA_KC_ADMIN_PASSWORD=admin
SUTRA_S3_ENDPOINT=http://localhost:9000
SUTRA_S3_BUCKET=sutraerp-documents
SUTRA_S3_ACCESS_KEY_ID=${SUTRA_COMPOSE_S3_ACCESS_KEY:?}
SUTRA_S3_SECRET_ACCESS_KEY=${SUTRA_COMPOSE_S3_SECRET_KEY:?}
SUTRA_PG_STAGING_DIR=$SUTRA_PG_STAGING_DIR
SUTRA_PG_STAGING_IN_CONTAINER=/backup-staging
SUTRA_WAL_ARCHIVE_DIR=$SUTRA_WAL_ARCHIVE_DIR
SUTRA_WAL_OFFSITE_DIR=$D/offsite/wal
SUTRA_WAL_LOCAL_KEEP_MIN=0
SUTRA_BACKUP_AGE_RECIPIENT=$rcpt
SUTRA_OFFSITE_TARGET=$D/offsite
SUTRA_COMPOSE_DIR=$root
EOF
  chmod 0600 "$ENVF"
  {
    echo "cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//')"
    echo "vcpu: $(nproc)"
    echo "ram: $(awk '/MemTotal/ { printf "%.1f GiB", $2 / 1048576 }' /proc/meminfo)"
    echo "disk: $(df -h / | awk 'NR == 2 { print $2 " total, " $4 " free" }')"
    echo "kernel: $(uname -r)"; echo "docker: $(docker version --format '{{.Server.Version}}')"
    echo "age: $(age --version)"; echo "aws: $(aws --version 2>&1)"; echo "pg_dump: $(pg_dump --version)"
  } | tee "$EV/runner.txt"
  ok "prepared $D (age recipient $rcpt; identity kept in $D/keys = off the server)"
  ;;

base)
  echo "SUTRA_PG_CONTAINER=$(pgc)" >>"$ENVF"
  timed backup "base backup (pg_basebackup + encrypt + offsite)" bash -c "bash '$B/sutra-fullbackup.sh' base | tail -1 >'$D/base-set.txt'"
  ok "base set $(cat "$D/base-set.txt")"
  ;;

ship-start)
  nohup bash "$B/sutra-wal-ship.sh" --loop 5 >"$EV/wal-ship.log" 2>&1 &
  echo $! >"$D/ship.pid"
  ok "WAL shipper running (pid $(cat "$D/ship.pid"), every 5 s)"
  ;;

tenant-b)
  # Synthetic SECOND tenant (fixture for the single-tenant restore test only): 25 document rows +
  # their objects, written directly (rows as superuser, objects through the S3 API).
  tb=0192f5a0-7c3e-7000-8000-00000000b0b0
  echo "$tb" >"$D/tenant-b.txt"
  set -a; . "$ENVF"; set +a
  export AWS_ACCESS_KEY_ID=$SUTRA_S3_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY=$SUTRA_S3_SECRET_ACCESS_KEY AWS_DEFAULT_REGION=us-east-1
  tmp=$(mktemp -d)
  for i in $(seq 1 25); do
    id=$(cat /proc/sys/kernel/random/uuid); owner=$(cat /proc/sys/kernel/random/uuid)
    key="$tb/$tb/drill_fixture/$owner/$id"
    { printf '%%PDF-1.7\n%% tenant B fixture %s\n' "$i"; head -c 6000 /dev/urandom | od -An -tx1 | tr -d ' \n' | fold -w 100 | sed 's/^/%/'; printf '\n%%%%EOF\n'; } >"$tmp/$i.pdf"
    sha=$(sha256sum "$tmp/$i.pdf" | cut -d' ' -f1); size=$(stat -c %s "$tmp/$i.pdf")
    aws --endpoint-url http://localhost:9000 --only-show-errors s3 cp "$tmp/$i.pdf" "s3://sutraerp-documents/$key" --content-type application/pdf
    psql "$PGURL" -qAtX -v ON_ERROR_STOP=1 -c "INSERT INTO platform.document_object (id, tenant_id, institution_id, owner_type, owner_id, filename, content_type, size_bytes, sha256, storage_key, storage_backend, uploaded_by, retention_until) VALUES ('$id', '$tb', '$tb', 'drill_fixture', '$owner', 'fixture-$i.pdf', 'application/pdf', $size, '$sha', '$key', 's3', 'drill', now() + interval '8 years')" >/dev/null
  done
  rm -rf "$tmp"
  ok "tenant B fixture: 25 documents ($tb)"
  ;;

quiesce)
  timed backup "quiesce writers (stop api + web)" docker compose stop api web
  psql "$PGURL" -qAtX -F ' ' -c "SELECT pg_database_size('sutra'), pg_database_size('keycloak'), (SELECT count(*) FROM platform.document_object), (SELECT coalesce(sum(size_bytes), 0) FROM platform.document_object), (SELECT count(*) FROM student.student)" >"$EV/sizes.txt"
  ok "api + web stopped (database writers quiesced; keycloak / minio still up); sizes $(cat "$EV/sizes.txt")"
  ;;

fingerprint)
  n=${1:?name}
  timed verify "fingerprint $n" bash -c "bash '$B/fingerprint.sh' '${2:-$PGURL}' '${3:-$KCURL}' >'$EV/fp-$n.txt'"
  ok "fingerprint $n: $(grep -c '^rows ' "$EV/fp-$n.txt") tables, $(awk '/^rows / { s += $3 } END { print s }' "$EV/fp-$n.txt") rows, $(grep '^docs ' "$EV/fp-$n.txt" | cut -d' ' -f2) documents, $(grep '^tb-total' "$EV/fp-$n.txt")"
  ;;

nightly)
  timed backup "nightly backup (dumps + objects + realm + inventory → encrypt → offsite)" bash -c "bash '$B/sutra-fullbackup.sh' nightly | tail -1 >'$D/nightly-set.txt'"
  s=$(cat "$D/nightly-set.txt")
  cp "$s/set.json" "$EV/nightly-set.json"
  ok "nightly set $(basename "$s"): $(du -sh "$s" | cut -f1)"
  ;;

heartbeat-start)
  psql "$PGURL" -qAtX -v ON_ERROR_STOP=1 -c "CREATE SCHEMA IF NOT EXISTS drill; CREATE TABLE IF NOT EXISTS drill.heartbeat (id bigserial PRIMARY KEY, at timestamptz NOT NULL DEFAULT clock_timestamp());"
  nohup bash -c "while :; do psql '$PGURL' -qAtX -F ' ' -c 'INSERT INTO drill.heartbeat DEFAULT VALUES RETURNING id, at' 2>/dev/null; sleep 1; done" >"$EV/heartbeat.log" 2>/dev/null &
  echo $! >"$D/heartbeat.pid"
  ok "heartbeat writer started (1 commit/s into drill.heartbeat; acknowledged rows → evidence/heartbeat.log)"
  ;;

disaster)
  # Crash, not shutdown: kill -9 PostgreSQL (WAL not yet archived is lost), the shipper dies with
  # the host, then EVERYTHING on the host goes: containers, volumes (pgdata, minio, nats), the WAL
  # archive, the staging area and the local backup disk. Only offsite/ and keys/ survive.
  [[ -s "$EV/heartbeat.log" ]] || die "no heartbeat acknowledged"
  date -u +%FT%T.%6NZ >"$D/disaster-at.txt"
  docker kill -s KILL "$(pgc)" >/dev/null
  kill "$(cat "$D/ship.pid")" 2>/dev/null || true
  sleep 2
  kill "$(cat "$D/heartbeat.pid")" 2>/dev/null || true
  tail -1 "$EV/heartbeat.log" >"$D/last-ack.txt"
  docker compose down -v --remove-orphans --timeout 5 >&2
  docker volume ls -q --filter label=com.docker.compose.project=sutraerp | xargs -r docker volume rm >/dev/null
  rm -rf "$D/host"
  left=$(docker volume ls -q --filter label=com.docker.compose.project=sutraerp | wc -l)
  [[ "$left" == 0 && ! -e "$D/host" ]] || die "host state survived the disaster"
  ok "DISASTER at $(cat "$D/disaster-at.txt"): postgres killed (-9), all containers + volumes + WAL archive + local backups deleted; last acknowledged heartbeat $(cat "$D/last-ack.txt")"
  ;;

restore)
  R=$D/restore
  rm -rf "$R"; mkdir -p "$R"
  echo "$(date -u +%FT%T.%3NZ)" >"$D/restore-start.txt"
  nb=$(basename "$(latest sutra-nightly)"); bb=$(basename "$(latest sutra-base)")
  [[ -n "$nb" && -n "$bb" ]] || die "no offsite sets"
  export SUTRA_BACKUP_AGE_IDENTITY=$D/keys/identity.txt
  # 1. fetch from offsite + verify checksums
  timed restore "1 fetch offsite sets + WAL, verify checksums" bash -c "
    bash '$B/offsite.sh' pull '$bb' '$R/in/$bb' && bash '$B/offsite.sh' pull '$nb' '$R/in/$nb' && bash '$B/offsite.sh' pull-wal '$R/in/wal'"
  # 2. decrypt
  timed restore "2 decrypt base + nightly + WAL (age)" bash -c "
    bash '$B/sutra-fullrestore.sh' decrypt '$R/in/$bb' '$R/base' && bash '$B/sutra-fullrestore.sh' decrypt '$R/in/$nb' '$R/nightly' && bash '$B/sutra-fullrestore.sh' wal '$R/in/wal' '$R/wal'"
  # 3. rebuilt host: directories, empty volumes, PGDATA from base + WAL
  install -d -o 999 -g 999 -m 0700 "$SUTRA_WAL_ARCHIVE_DIR" "$SUTRA_PG_STAGING_DIR"
  install -d -m 0700 "$D/host/backups"
  timed restore "3 prepare PGDATA (base backup + WAL to replay)" bash -c "
    docker compose create postgres >/dev/null 2>&1 && bash '$B/sutra-fullrestore.sh' pgdata sutraerp_pgdata '$R/base' '$R/wal'"
  # 4. start postgres → WAL replay to the end of the archive → promote
  timed restore "4 postgres up + WAL replay (archive recovery) + promote" bash -c "
    docker compose up -d --no-deps postgres >/dev/null && bash '$B/sutra-fullrestore.sh' wait-recovery \"\$(docker compose ps -q postgres)\" 1800 | tee '$EV/recovered-to.txt'"
  # 5. consistency of the database BEFORE any application writes
  bash "$here/drill.sh" fingerprint after-restore
  timed verify "fingerprint compare (before disaster vs restored)" bash "$B/fingerprint.sh" --compare "$EV/fp-before.txt" "$EV/fp-after-restore.txt"
  # 6. infrastructure services + document objects
  timed restore "6 keycloak, minio, nats, valkey, mailpit up (healthy)" docker compose up -d --wait --no-deps keycloak minio nats valkey mailpit
  timed restore "7 restore document objects + verify every sha256 in MinIO" bash "$B/sutra-fullrestore.sh" objects "$R/nightly"
  # 8. application
  timed restore "8 migrate + api + web up (all health checks)" bash -c "docker compose up -d >/dev/null && bash deploy/ci/demo-wait.sh >'$EV/health-after-restore.txt'"
  # 9. application-level verification
  DRILL_INSTITUTION=$(jq -r .institutionId "$EV/drill-seed.json" 2>/dev/null || true)
  export DRILL_INSTITUTION
  timed restore "9 verify: Keycloak sign-ins, audit chain, document downloads, a new write" node "$B/drill/verify.mjs"
  echo "$(date -u +%FT%T.%3NZ)" >"$D/restore-end.txt"
  ok "restore complete: service verified"
  ;;

rpo)
  # Achieved RPO = last heartbeat acknowledged to the client before the crash − newest heartbeat
  # that came back (commits in between were lost: their WAL was not yet archived + shipped).
  read -r ack_id ack_at <"$D/last-ack.txt"
  got=$(psql "$PGURL" -qAtX -F ' ' -c "SELECT max(id), max(at) FROM drill.heartbeat")
  read -r rid rat <<<"$got"
  python3 - "$ack_id" "$ack_at" "$rid" "$rat" "$D/disaster-at.txt" "$D/offsite/wal/ship.log" "$EV/rpo.json" <<'PY'
import json, sys, datetime as dt
ack_id, ack_at, rid, rat, dis_f, ship_f, out = sys.argv[1:]
p = lambda s: dt.datetime.fromisoformat(s.strip().replace("Z", "+00:00").replace(" ", "T"))
dis = p(open(dis_f).read())
ships = [l.split() for l in open(ship_f) if l.strip()]
segs = [s for s in ships if len(s[0]) == 24]
last = max(segs, key=lambda s: s[2]) if segs else None
r = {
  "lastAcknowledgedCommit": {"id": int(ack_id), "at": ack_at},
  "lastRecoveredCommit": {"id": int(rid), "at": rat},
  "lostCommits": int(ack_id) - int(rid),
  "achievedRpoSeconds": round((p(ack_at) - p(rat)).total_seconds(), 1),
  "disasterAt": dis.isoformat(),
  "lastSegmentShipped": {"file": last[0], "archivedAt": last[1], "shippedAt": last[2]} if last else None,
  "secondsFromLastShipToDisaster": round((dis - p(last[2])).total_seconds(), 1) if last else None,
  "segmentsShipped": len(segs),
  "archiveTimeoutSeconds": 60, "shipIntervalSeconds": 5,
}
json.dump(r, open(out, "w"), indent=2)
print(f"ok achieved RPO {r['achievedRpoSeconds']} s ({r['lostCommits']} heartbeat commits lost; last recovered #{rid} at {rat}, last acknowledged #{ack_id} at {ack_at})")
PY
  ;;

pitr)
  # Point-in-time recovery to a chosen timestamp: halfway between two acknowledged heartbeats in
  # the middle of the post-backup window → exactly the earlier one (and nothing after it) back.
  R=$D/restore
  read -r tid target < <(python3 - "$EV/heartbeat.log" <<'PY'
import sys, datetime as dt
rows = [l.split(" ", 1) for l in open(sys.argv[1]) if l.strip()]
p = lambda s: dt.datetime.fromisoformat(s.strip().replace(" ", "T"))
k = len(rows) // 2
a, b = p(rows[k][1]), p(rows[k + 1][1])
t = a + (b - a) / 2
print(rows[k][0], t.strftime("%Y-%m-%d %H:%M:%S.%f+00"))
PY
)
  echo "$tid $target" >"$EV/pitr-target.txt"
  docker rm -f sutra-pitr >/dev/null 2>&1 || true
  docker volume rm sutra-pitr-data >/dev/null 2>&1 || true
  timed pitr "prepare PGDATA with recovery_target_time" bash "$B/sutra-fullrestore.sh" pgdata sutra-pitr-data "$R/base" "$R/wal" --target-time "$target"
  timed pitr "postgres up + replay to the target + promote" bash -c "
    docker run -d --name sutra-pitr -p 5433:5432 -v sutra-pitr-data:/var/lib/postgresql postgres:18 >/dev/null && bash '$B/sutra-fullrestore.sh' wait-recovery sutra-pitr 900 | tee '$EV/pitr-recovered-to.txt'"
  P=postgres://sutra:sutra@localhost:5433/sutra
  got=$(psql "$P" -qAtX -c "SELECT max(id) FROM drill.heartbeat")
  after=$(psql "$P" -qAtX -c "SELECT count(*) FROM drill.heartbeat WHERE at > '$target'")
  [[ "$got" == "$tid" && "$after" == 0 ]] || die "PITR to $target: newest heartbeat #$got (expected #$tid), $after rows after the target"
  ok "PITR to $target: heartbeat #$tid is the newest row, nothing after the target"
  bash "$here/drill.sh" fingerprint pitr "$P" "postgres://sutra:sutra@localhost:5433/keycloak"
  timed verify "fingerprint compare (before disaster vs PITR)" bash "$B/fingerprint.sh" --compare "$EV/fp-before.txt" "$EV/fp-pitr.txt"
  docker rm -f sutra-pitr >/dev/null; docker volume rm sutra-pitr-data >/dev/null
  ;;

logical)
  # The nightly logical set restored with the slice-9C tooling into a fresh, EMPTY PostgreSQL 18
  # (another server): checksums → roles → pg_restore → migrate → counts / audit heads / grants vs
  # the manifest, Keycloak dump → keycloak DB; then the fingerprint equals the pre-disaster one.
  R=$D/restore
  docker rm -f sutra-logical >/dev/null 2>&1 || true
  docker run -d --name sutra-logical -p 5434:5432 -e POSTGRES_USER=sutra -e POSTGRES_PASSWORD=sutra -e POSTGRES_DB=sutra \
    -v "$root/deploy/postgres/init.sql:/docker-entrypoint-initdb.d/00-init.sql:ro" postgres:18 >/dev/null
  for _ in $(seq 1 60); do docker exec sutra-logical pg_isready -U sutra -d sutra >/dev/null 2>&1 && psql postgres://sutra:sutra@localhost:5434/sutra -qAtX -c 'SELECT 1' >/dev/null 2>&1 && break; sleep 2; done
  L=postgres://sutra:sutra@localhost:5434/sutra
  timed logical "restore nightly logical set (sutra-restore.sh + keycloak dump)" env \
    SUTRA_BACKUP_DATABASE_URL=$L SUTRA_BACKUP_KEYCLOAK_DATABASE_URL=postgres://sutra:sutra@localhost:5434/keycloak \
    SUTRA_API_BIN="${SUTRA_API_BIN:-$root/target/debug/sutra-api}" SUTRA_ENV=development \
    SUTRA_DB_RUNTIME_USER=sutra_api SUTRA_DB_RUNTIME_PASSWORD=sutra-api-dev-only \
    bash -c "bash '$B/sutra-fullrestore.sh' logical '$R/nightly' | tee '$EV/logical-restore.log'"
  bash "$here/drill.sh" fingerprint logical "$L" "postgres://sutra:sutra@localhost:5434/keycloak"
  timed verify "fingerprint compare (before disaster vs logical restore)" bash "$B/fingerprint.sh" --compare "$EV/fp-before.txt" "$EV/fp-logical.txt"
  ;;

corrupt)
  # Partial failures: each damaged copy must be REFUSED, naming what is wrong.
  R=$D/restore
  export SUTRA_BACKUP_AGE_IDENTITY=$D/keys/identity.txt
  nb=$(basename "$(latest sutra-nightly)")
  C=$D/corrupt; rm -rf "$C"; mkdir -p "$C"
  cp -a "$D/offsite/sets/$nb" "$C/set"
  flip_byte "$C/set/objects.tar.age" 4096
  expect_fail "encrypted-file-checksum" "checksum mismatch|FAILED" bash "$B/sutra-backup-verify.sh" "$C/set"
  # attacker / bit rot that also rewrote SHA256SUMS: age's authenticated encryption still refuses it
  (cd "$C/set" && sha256sum -- *.age >SHA256SUMS)
  expect_fail "encrypted-file-authentication" "decryption failed|age:" bash "$B/sutra-fullrestore.sh" decrypt "$C/set" "$C/plain"
  # damaged dump inside a decrypted set: sutra-restore.sh refuses it BEFORE touching the database
  mkdir -p "$C/db" && tar -C "$C/db" -xf "$R/nightly/database.tar"
  dd=$(ls -d "$C"/db/sutra-backup-*)
  flip_byte "$dd/sutra.dump" 20000
  tar -C "$C/db" -cf "$C/database-bad.tar" "$(basename "$dd")"
  expect_fail "database-dump-checksum" "sha256 mismatch" env SUTRA_BACKUP_DATABASE_URL=postgres://sutra:sutra@localhost:5434/sutra \
    bash "$B/sutra-restore.sh" "$C/database-bad.tar" --skip-migrate
  # one damaged document object in the mirror: detected, named
  mkdir -p "$C/obj" && tar -C "$C/obj" -xf "$R/nightly/objects.tar"
  victim=$(find "$C/obj/objects" -type f | LC_ALL=C sort | sed -n 7p)
  flip_byte "$victim" 300
  expect_fail "document-object-sha256" "SHA256 MISMATCH .*${victim##*/}" bash "$B/sutra-objects.sh" verify "$C/obj" "$C/obj/documents.tsv"
  # one damaged WAL segment offsite
  cp -a "$D/offsite/wal" "$C/wal"
  seg=$(ls -1 "$C/wal" | grep -E '^[0-9A-F]{24}\.age$' | LC_ALL=C sort | sed -n 2p)
  flip_byte "$C/wal/$seg" 100
  expect_fail "wal-segment-checksum" "checksum mismatch|FAILED" bash "$B/sutra-fullrestore.sh" wal "$C/wal" "$C/walplain"
  rm -rf "$C"
  ok "partial-failure tests: 5/5 damaged inputs detected"
  ;;

tenant-restore)
  # Single-tenant document restore on the LIVE restored stack: tenant B's objects are deleted,
  # restored alone from the nightly set; tenant A's objects are untouched (same keys + ETags).
  tb=$(cat "$D/tenant-b.txt")
  ta=$(psql "$PGURL" -qAtX -c "SELECT tenant_id FROM platform.document_object WHERE tenant_id <> '$tb' GROUP BY 1 ORDER BY count(*) DESC LIMIT 1")
  set -a; . "$ENVF"; set +a
  tmp=$(mktemp -d)
  tar -C "$tmp" -xf "$D/restore/nightly/objects.tar"
  bash "$B/sutra-objects.sh" listing --tenant "$ta" >"$EV/tenant-a-before.txt"
  bash "$B/sutra-objects.sh" purge-tenant "$tb"
  expect_fail "tenant-b-missing-after-purge" "missing" bash "$B/sutra-objects.sh" verify-live "$tmp/documents.tsv" --tenant "$tb"
  timed restore "single-tenant document restore (tenant B)" bash "$B/sutra-objects.sh" restore "$tmp" --tenant "$tb"
  bash "$B/sutra-objects.sh" verify-live "$tmp/documents.tsv" --tenant "$tb"
  bash "$B/sutra-objects.sh" listing --tenant "$ta" >"$EV/tenant-a-after.txt"
  cmp -s "$EV/tenant-a-before.txt" "$EV/tenant-a-after.txt" || die "tenant A objects changed by the tenant B restore"
  ok "tenant A untouched ($(wc -l <"$EV/tenant-a-after.txt") objects, keys + ETags identical)"
  rm -rf "$tmp"
  ;;

units)
  F=$D/fakeroot; rm -rf "$F"
  SUTRA_INSTALL_ROOT=$F bash "$B/systemd/install.sh"
  if command -v systemd-analyze >/dev/null; then
    (cd "$F/etc/systemd/system" && systemd-analyze verify --root="$F" sutraerp-backup.service sutraerp-backup.timer \
      sutraerp-wal-ship.service sutraerp-wal-ship.timer sutraerp-backup-verify.service sutraerp-backup-verify.timer 2>&1 \
      | grep -v -E 'docker.service|network-online|Failed to prepare filename|sutraerp-backup-alert@' | tee "$EV/systemd-verify.txt") || true
    for t in sutraerp-backup.timer sutraerp-wal-ship.timer sutraerp-backup-verify.timer; do
      c=$(sed -n 's/^OnCalendar=//p' "$F/etc/systemd/system/$t")
      [[ -z "$c" ]] || systemd-analyze calendar --iterations=2 "$c" | tee -a "$EV/systemd-calendar.txt" >/dev/null
    done
  fi
  bash "$B/rotate.sh" "$D/offsite/sets" --wal-dir "$D/offsite/wal" --dry-run | tee "$EV/rotate-dry-run.txt"
  ok "units installed into a scratch root + verified; rotation dry run on the offsite sets"
  ;;

report)
  python3 "$here/report.py" "$D" | tee "$EV/summary.md"
  [[ -n "${GITHUB_STEP_SUMMARY:-}" ]] && cat "$EV/summary.md" >>"$GITHUB_STEP_SUMMARY"
  ;;

*) die "unknown command $cmd" ;;
esac
