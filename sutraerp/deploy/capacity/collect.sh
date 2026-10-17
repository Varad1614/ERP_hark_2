#!/usr/bin/env bash
# Part 2 · S5-capacity: resource + database evidence around the load (deploy/capacity/README.md).
#
#   collect.sh start <out-dir>     background samplers: docker stats (every 5 s), PostgreSQL
#                                  connections / waits / locks (every 10 s)
#   collect.sh stop <out-dir>      stops them
#   collect.sh snapshot <out-dir> <label>
#                                  database size + biggest tables, cache hit ratio, deadlocks,
#                                  temp files, Docker volume sizes, host disk, API /metrics
#   collect.sh statements <out-dir> [reset]
#                                  pg_stat_statements top queries (total / mean time) as JSON
# Env: PGURL (schema owner, sslmode=require), COMPOSE (the docker compose command line).
set -uo pipefail
cmd="$1"; out="$2"; mkdir -p "$out"
PGURL="${PGURL:?set PGURL}"
q() { psql "$PGURL" -XAtq -v ON_ERROR_STOP=1 -c "$1"; }

case "$cmd" in
  start)
    date -u +%s >"$out/load-start.epoch"
    (
      while :; do
        ts=$(date -u +%s)
        docker stats --no-stream --format '{{json .}}' 2>/dev/null | sed "s/^/{\"ts\":$ts,\"s\":/; s/\$/}/"
        sleep 4
      done
    ) >>"$out/docker-stats.jsonl" 2>/dev/null &
    echo $! >"$out/.stats.pid"
    (
      while :; do
        q "SELECT json_build_object('ts', extract(epoch from now())::bigint,
             'conns', (SELECT json_object_agg(coalesce(state,'none'), n) FROM (SELECT state, count(*) n FROM pg_stat_activity WHERE backend_type = 'client backend' GROUP BY state) x),
             'byUser', (SELECT json_object_agg(usename, n) FROM (SELECT usename, count(*) n FROM pg_stat_activity WHERE backend_type = 'client backend' GROUP BY usename) y),
             'waiting', (SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock'),
             'waits', (SELECT json_object_agg(w, n) FROM (SELECT coalesce(wait_event_type,'CPU') || ':' || coalesce(wait_event,'-') w, count(*) n FROM pg_stat_activity WHERE state = 'active' AND backend_type = 'client backend' AND pid <> pg_backend_pid() GROUP BY 1) z),
             'locks', (SELECT json_object_agg(mode, n) FROM (SELECT mode, count(*) n FROM pg_locks GROUP BY mode) l),
             'xact_commit', (SELECT sum(xact_commit) FROM pg_stat_database),
             'blks_hit', (SELECT sum(blks_hit) FROM pg_stat_database), 'blks_read', (SELECT sum(blks_read) FROM pg_stat_database))" 2>/dev/null
        sleep 10
      done
    ) >>"$out/pg-samples.jsonl" 2>/dev/null &
    echo $! >"$out/.pg.pid"
    echo "ok collectors started"
    ;;
  stop)
    for f in "$out"/.stats.pid "$out"/.pg.pid; do [[ -f "$f" ]] && kill "$(cat "$f")" 2>/dev/null; rm -f "$f"; done
    date -u +%s >"$out/load-stop.epoch"
    echo "ok collectors stopped"
    ;;
  snapshot)
    label="$3"
    {
      echo "{"
      echo "\"label\": \"$label\", \"ts\": $(date -u +%s),"
      echo "\"dbBytes\": $(q "SELECT pg_database_size('sutra')"),"
      echo "\"keycloakDbBytes\": $(q "SELECT pg_database_size('keycloak')"),"
      echo "\"flowableDbBytes\": $(q "SELECT coalesce(pg_database_size('flowable'),0)"),"
      echo "\"tables\": $(q "SELECT coalesce(json_agg(t), '[]') FROM (SELECT n.nspname || '.' || c.relname AS name, pg_total_relation_size(c.oid) AS bytes, c.reltuples::bigint AS rows FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relkind = 'r' AND n.nspname NOT IN ('pg_catalog','information_schema') ORDER BY 2 DESC LIMIT 20) t"),"
      echo "\"cache\": $(q "SELECT json_build_object('hit', sum(blks_hit), 'read', sum(blks_read), 'ratio', round(sum(blks_hit)::numeric / nullif(sum(blks_hit) + sum(blks_read), 0), 5), 'deadlocks', sum(deadlocks), 'tempFiles', sum(temp_files), 'tempBytes', sum(temp_bytes), 'commits', sum(xact_commit), 'rollbacks', sum(xact_rollback)) FROM pg_stat_database WHERE datname = 'sutra'"),"
      echo "\"seqScans\": $(q "SELECT coalesce(json_agg(t), '[]') FROM (SELECT schemaname || '.' || relname AS name, seq_scan, seq_tup_read, idx_scan, n_live_tup FROM pg_stat_user_tables WHERE seq_scan > 0 ORDER BY seq_tup_read DESC LIMIT 15) t"),"
      echo "\"volumes\": {$(for v in $(docker volume ls -q | grep '^sutracap_'); do printf '"%s": %s,' "$v" "$(sudo du -sb "$(docker volume inspect -f '{{.Mountpoint}}' "$v")" 2>/dev/null | cut -f1 || echo 0)"; done | sed 's/,$//')},"
      echo "\"hostDisk\": \"$(df -B1 --output=used,avail / | tail -1 | xargs)\""
      echo "}"
    } >"$out/snapshot-$label.json"
    curl -fsS -m 10 http://127.0.0.1:9464/metrics >"$out/api-metrics-$label.txt" 2>/dev/null || true
    echo "ok snapshot $label"
    ;;
  statements)
    [[ "${3:-}" == reset ]] && { q "SELECT pg_stat_statements_reset()" >/dev/null; echo "ok pg_stat_statements reset"; exit 0; }
    q "SELECT json_build_object(
         'byTotal', (SELECT json_agg(t) FROM (SELECT left(regexp_replace(query, '\s+', ' ', 'g'), 400) AS query, calls, round(total_exec_time::numeric, 1) AS total_ms, round(mean_exec_time::numeric, 3) AS mean_ms, round(max_exec_time::numeric, 1) AS max_ms, rows, shared_blks_hit, shared_blks_read FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = 'sutra') ORDER BY total_exec_time DESC LIMIT 30) t),
         'byMean', (SELECT json_agg(t) FROM (SELECT left(regexp_replace(query, '\s+', ' ', 'g'), 400) AS query, calls, round(total_exec_time::numeric, 1) AS total_ms, round(mean_exec_time::numeric, 3) AS mean_ms, round(max_exec_time::numeric, 1) AS max_ms, rows FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = 'sutra') AND calls >= 20 ORDER BY mean_exec_time DESC LIMIT 20) t),
         'totals', (SELECT json_build_object('calls', sum(calls), 'total_ms', round(sum(total_exec_time)::numeric, 1)) FROM pg_stat_statements WHERE dbid = (SELECT oid FROM pg_database WHERE datname = 'sutra')))" >"$out/pg-statements.json"
    echo "ok pg_stat_statements → $out/pg-statements.json"
    ;;
  *) echo "usage: collect.sh start|stop|snapshot|statements <out> [label]" >&2; exit 2 ;;
esac
