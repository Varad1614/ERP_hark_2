#!/usr/bin/env bash
# Staging drill A (Part 2 · S5-deploy): fresh offline install of the current release bundle like a
# college administrator, smoke journey, service restarts, host-reboot simulation, crash recovery.
#
#   OUT=<results dir> run-fresh.sh <bundle .tar>
#
# Writes $OUT/{host-facts-*.md,json, timings.tsv, results.tsv, resources.md, smoke.log, chaos.tsv,
# logs/}. Fails on the first broken expectation.
set -euo pipefail
# shellcheck source=common.sh
. "$(dirname "${BASH_SOURCE[0]}")/common.sh"
tarball=${1:?run-fresh.sh <bundle tar>}

say "host facts"
bash "$CI_DIR/host-facts.sh" "$OUT" staging-runner-fresh
sudo ss -tlnp >"$OUT/ports-before.txt" 2>&1 || true

say "unpack the bundle (checksum of the tar), go offline"
bundle=$(unpack_bundle "$tarball" /tmp/onprem)
go_offline
staging_dns

say "install (offline, from the bundle only)"
t0=$(now)
sudo -E "$bundle/bin/sutra-onprem" install --domain "$ERP_HOST" --sso-domain "$SSO_HOST" --local-ca
record install_wall "$(since "$t0")"
sudo ss -tlnp >"$OUT/ports-after.txt" 2>&1 || true
result "published ports" "$(sudo ss -Htln | awk '{ print $4 }' | grep -E ':(80|443|9000)$' | sort -u | tr '\n' ' ')"
$SO health
# TLS facts as a browser sees them.
echo | openssl s_client -connect 127.0.0.1:443 -servername "$ERP_HOST" -CAfile /etc/sutraerp/tls/local-ca/ca.crt 2>/dev/null |
  grep -E 'Verify return code|Protocol|Cipher is' | tee "$OUT/tls.txt"
grep -q 'Verify return code: 0' "$OUT/tls.txt" || { echo "TLS verification failed" >&2; exit 1; }
result tls "certificate for $ERP_HOST verifies against the local CA ($(grep -m1 -oE "TLSv1\.[23]" "$OUT/tls.txt" || echo "protocol n/a"))"
code=$(curl -s -o /dev/null -w '%{http_code}' "http://$ERP_HOST/")
[[ "$code" == 301 ]] || { echo "http did not redirect ($code)" >&2; exit 1; }
result http-redirect "http://$ERP_HOST → 301 https"
hsts=$(curl -sI --cacert /etc/sutraerp/tls/local-ca/ca.crt "https://$ERP_HOST/" | grep -i strict-transport || true)
result hsts "${hsts:-missing}"
resources installed-idle

say "first tenant + people, smoke journey (login, student, fee, document)"
bootstrap_people
smoke seed
smoke document
smoke verify
c_ref=$(counts)
result counts-after-smoke "$c_ref"
resources after-smoke

say "restart every service, one at a time"
for s in postgres keycloak valkey nats minio flowable api web edge; do
  # sutra-onprem records restart_<svc> itself (SUTRA_TIMINGS_FILE).
  $SO restart "$s" >/dev/null
done
same_data "$c_ref" "$(counts)" "after restarting each service"

say "restart all containers at once (docker restart)"
t0=$(now)
docker restart $(docker ps -q) >/dev/null
$SO health --wait 900
record restart_all_containers "$(since "$t0")"
same_data "$c_ref" "$(counts)" "after docker restart of all containers"

say "host reboot simulation: restart the Docker daemon (containers come back by restart policy)"
t0=$(now)
sudo systemctl restart docker
$SO health --wait 900
record reboot_sim_docker_daemon "$(since "$t0")"
same_data "$c_ref" "$(counts)" "after Docker daemon restart"
SMOKE_SUFFIX=_after_reboot_sim smoke verify

say "crash recovery: SIGKILL the main process of each service"
printf 'service\tkilled_at\trecovered_s\trestart_count\tcheck\n' >"$OUT/chaos.tsv"
for s in postgres api valkey nats minio keycloak; do
  before=$(counts)
  cid=$(docker ps -q --filter "label=com.docker.compose.project=sutraerp" --filter "label=com.docker.compose.service=$s")
  rc0=$(docker inspect -f '{{.RestartCount}}' "$cid")
  pid=$(docker inspect -f '{{.State.Pid}}' "$cid")
  t0=$(now)
  sudo kill -9 "$pid"
  for _ in $(seq 1 300); do
    [[ "$(docker inspect -f '{{.RestartCount}}' "$cid")" != "$rc0" ]] && break
    sleep 0.2
  done
  # Recovered = the container restarted by its policy, every health check green again (API ready,
  # SSO discovery, web) and the service's own probe passes.
  $SO health --wait 600
  case "$s" in
    minio) $SO bucket >/dev/null; probe="bucket private + reachable over TLS" ;;
    keycloak) curl -fsS --cacert /etc/sutraerp/tls/local-ca/ca.crt "https://$SSO_HOST/realms/sutra/.well-known/openid-configuration" >/dev/null; probe="OIDC discovery" ;;
    *) probe="API ready" ;;
  esac
  rec=$(since "$t0")
  rc1=$(docker inspect -f '{{.RestartCount}}' "$cid")
  printf '%s\t%s\t%s\t%s\t%s\n' "$s" "$(date -u +%T)" "$rec" "$rc0→$rc1" "$probe" >>"$OUT/chaos.tsv"
  record "recover_kill_$s" "$rec"
  same_data "$before" "$(counts)" "after SIGKILL of $s"
done
SMOKE_SUFFIX=_after_chaos smoke verify
column -t -s $'\t' "$OUT/chaos.tsv"

say "backup (timing + size)"
bdir=$($SO backup --label staging | tail -1)
result backup "$bdir $(sudo du -sh "$bdir" | cut -f1): $(sudo ls "$bdir" | tr '\n' ' ')"
sudo du -sh /var/lib/docker >"$OUT/docker-disk.txt" 2>&1 || true
resources final

say "done"
collect_logs
column -t -s $'\t' "$SUTRA_TIMINGS_FILE"
