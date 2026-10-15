#!/usr/bin/env bash
# kind end-to-end (9B): cluster → OpenBao dev + seed → helm install → port-forwards → stage scripts.
# Used by the `Kind (install · migrate · smoke · telemetry · DR)` job and runnable locally.
# Expects the images ghcr.io/varad1614/sutraerp-{api,web}:ci in the local docker.
# Env: KIND_NODE_IMAGE (pinned in the workflow), SUTRA_CI_NS (sutraerp), KIND_SKIP_CREATE=1 to reuse
# a cluster, KIND_SKIP_STAGES=1 to stop after the install.
set -euo pipefail
cd "$(dirname "$0")/../.."          # sutraerp/

NS=${SUTRA_CI_NS:-sutraerp}
CLUSTER=sutraerp
NODE_IMAGE=${KIND_NODE_IMAGE:-kindest/node:v1.31.14@sha256:6f86cf509dbb42767b6e79debc3f2c32e4ee01386f0489b3b2be24b0a55aac2b}
OPENBAO_CHART_VERSION=0.30.3
export KUBECONFIG=${KUBECONFIG:-$HOME/.kube/config}

step() { echo "::group::$*"; }
endstep() { echo "::endgroup::"; }

if [[ "${KIND_SKIP_CREATE:-}" != 1 ]]; then
  step "kind create cluster"
  kind get clusters | grep -qx "$CLUSTER" || kind create cluster --config deploy/kind/cluster.yaml --image "$NODE_IMAGE" --wait 180s
  endstep
fi

step "load images"
for img in ghcr.io/varad1614/sutraerp-api:ci ghcr.io/varad1614/sutraerp-web:ci; do
  kind load docker-image --name "$CLUSTER" "$img"
done
endstep

step "OpenBao (dev) + injector"
BAO_TOKEN=${BAO_TOKEN:-$(openssl rand -hex 16)}
export BAO_TOKEN
if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::add-mask::$BAO_TOKEN"; fi
helm repo add openbao https://openbao.github.io/openbao-helm >/dev/null 2>&1 || true
helm repo update openbao >/dev/null
kubectl create namespace "$NS" --dry-run=client -o yaml | kubectl apply -f - >/dev/null
helm upgrade --install openbao openbao/openbao --version "$OPENBAO_CHART_VERSION" -n openbao --create-namespace \
  -f deploy/openbao/values-dev.yaml --set server.dev.devRootToken="$BAO_TOKEN" --wait --timeout 5m
kubectl -n openbao rollout status deploy/openbao-agent-injector --timeout=180s
endstep

step "seed OpenBao"
SUTRA_CI_NS=$NS SUTRA_ENV=development bash deploy/openbao/seed-dev.sh
endstep

step "helm install sutraerp"
start=$(date +%s)
helm upgrade --install sutraerp deploy/helm/sutraerp -n "$NS" --create-namespace \
  -f deploy/helm/sutraerp/ci/kind.yaml --wait --wait-for-jobs --timeout 15m
echo "helm install took $(( $(date +%s) - start ))s"
kubectl get pods -n "$NS" -o wide
endstep

step "helm upgrade (re-install is idempotent: new migrate revision, same OpenBao values)"
SUTRA_CI_NS=$NS SUTRA_ENV=development bash deploy/openbao/seed-dev.sh >/dev/null
helm upgrade sutraerp deploy/helm/sutraerp -n "$NS" -f deploy/helm/sutraerp/ci/kind.yaml --wait --wait-for-jobs --timeout 10m
endstep

step "port-forwards"
pf() { # svc local:remote...
  local svc=$1; shift
  kubectl -n "$NS" port-forward "svc/$svc" "$@" >"/tmp/pf-$svc.log" 2>&1 &
}
pf sutraerp-api 8080:8080
pf sutraerp-web 8081:8080
if kubectl -n "$NS" get svc sutraerp-lgtm >/dev/null 2>&1; then
  pf sutraerp-lgtm 3000:3000 9090:9090 3100:3100 3200:3200 4318:4318
fi
for _ in $(seq 1 30); do curl -fs http://localhost:8080/health/live >/dev/null && break; sleep 1; done
endstep

export SUTRA_CI_MODE=kind SUTRA_CI_NS=$NS
# INT: the bundled PostgreSQL superuser password lives only in OpenBao (database group); the DR
# drill connects through a port-forward with it (dr-drill.sh reads SUTRA_CI_PG_PASSWORD).
SUTRA_CI_PG_PASSWORD=$(kubectl exec -n openbao openbao-0 -- env BAO_ADDR=http://127.0.0.1:8200 BAO_TOKEN="$BAO_TOKEN" \
  bao kv get -mount=kv -field=POSTGRES_PASSWORD "sutraerp/development/database")
if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::add-mask::$SUTRA_CI_PG_PASSWORD"; fi
export SUTRA_CI_PG_PASSWORD
export SUTRA_CI_API_URL=http://localhost:8080 SUTRA_CI_WEB_URL=http://localhost:8081
export SUTRA_CI_GRAFANA_URL=http://localhost:3000 SUTRA_CI_PROMETHEUS_URL=http://localhost:9090
export SUTRA_CI_LOKI_URL=http://localhost:3100 SUTRA_CI_TEMPO_URL=http://localhost:3200

if [[ "${KIND_SKIP_STAGES:-}" == 1 ]]; then exit 0; fi
for stage in smoke check-telemetry seed-pilot dr-drill; do
  step "stage $stage"
  bash "deploy/ci/$stage.sh"
  endstep
done
echo "ok kind all-stages"
