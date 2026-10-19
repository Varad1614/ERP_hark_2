#!/usr/bin/env bash
# Builds the offline on-prem release bundle (Part 2 · S5-deploy) on a machine WITH registry access
# (the CI runner). The college server installs from the bundle only (no registry, no git).
#
#   build-bundle.sh --version 2026.10.1-ab12cd34 --git-sha <sha> --release-src <sutraerp tree of the release> \
#     --api-image sutraerp-api:<v> --web-image sutraerp-web:<v> --minio-tag RELEASE.… \
#     --private-ca-trust true|false --documents true|false --out dist/
#
# Output: dist/sutraerp-onprem-<version>/ and dist/sutraerp-onprem-<version>.tar:
#   bundle.env  RELEASE.json  VERSION  SHA256SUMS
#   images/*.tar.gz   every image the stack runs (docker save | gzip), digests in RELEASE.json
#   sbom/*.spdx.json  SPDX SBOM per image (syft), when syft is installed
#   bin/sutra-onprem  compose/ (compose.yaml, edge.conf.template, postgres-init/)  config/
#   keycloak/realm-production-baseline.json  backup/ (9C backup/restore scripts)  helm/sutraerp/
#   docs/ (DEPLOYMENT_RUNBOOK.md, INFRASTRUCTURE_REPORT.md)
# Tools (bin, compose, config) come from THIS tree (deploy/onprem); the realm, backup scripts and
# chart come from the release tree (what that release shipped).
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
tools=$(cd "$here/../.." && pwd) # sutraerp/ of the tooling checkout

version="" sha="" src="" api="" web="" minio_tag="" ca_trust=false docs=false out=dist
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) version=$2; shift 2 ;;
    --git-sha) sha=$2; shift 2 ;;
    --release-src) src=$2; shift 2 ;;
    --api-image) api=$2; shift 2 ;;
    --web-image) web=$2; shift 2 ;;
    --minio-tag) minio_tag=$2; shift 2 ;;
    --private-ca-trust) ca_trust=$2; shift 2 ;;
    --documents) docs=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
for v in version sha src api web minio_tag; do [[ -n "${!v}" ]] || { echo "--${v//_/-} is required" >&2; exit 2; }; done
src=$(cd "$src" && pwd)

THIRD_PARTY=(
  postgres:18
  quay.io/keycloak/keycloak:26.7
  valkey/valkey:8
  nats:2.11-alpine
  flowable/flowable-rest:8.0.0
  nginxinc/nginx-unprivileged:1.30-alpine
)
IMAGES=("$api" "$web" "sutraerp-minio:$minio_tag" "${THIRD_PARTY[@]}")

name="sutraerp-onprem-$version"
mkdir -p "$out"
out=$(cd "$out" && pwd)
b="$out/$name"
rm -rf "$b"
mkdir -p "$b"/{images,sbom,bin,compose/postgres-init,config,keycloak,backup,helm,docs}

for img in "${THIRD_PARTY[@]}"; do
  docker image inspect "$img" >/dev/null 2>&1 || docker pull -q "$img" >/dev/null
done

echo "== files"
install -m 755 "$tools/deploy/onprem/bin/sutra-onprem" "$b/bin/sutra-onprem"
install -m 644 "$tools/deploy/onprem/compose/compose.yaml" "$tools/deploy/onprem/compose/edge.conf.template" "$b/compose/"
install -m 755 "$tools/deploy/onprem/compose/postgres-init/10-sutra.sh" "$b/compose/postgres-init/"
install -m 755 "$src/deploy/flowable/init-db.sh" "$b/compose/postgres-init/20-flowable.sh"
install -m 644 "$tools/deploy/onprem/config/sutraerp.conf.example" "$b/config/"
install -m 644 "$src/deploy/keycloak/realm-production-baseline.json" "$b/keycloak/"
cp -a "$src/deploy/backup/." "$b/backup/"
cp -a "$src/deploy/helm/sutraerp" "$b/helm/"
for d in DEPLOYMENT_RUNBOOK.md INFRASTRUCTURE_REPORT.md; do
  if [[ -f "$tools/docs/readiness/$d" ]]; then install -m 644 "$tools/docs/readiness/$d" "$b/docs/"; fi
done

cat >"$b/bundle.env" <<EOF
SUTRA_VERSION=$version
SUTRA_GIT_SHA=$sha
SUTRA_MINIO_TAG=$minio_tag
SUTRA_BUNDLE_PRIVATE_CA_TRUST=$ca_trust
SUTRA_BUNDLE_DOCUMENTS=$docs
EOF
echo "$version" >"$b/VERSION"

echo "== images"
: >"$b/images/IMAGES.tsv"
for img in "${IMAGES[@]}"; do
  f=$(echo "$img" | tr '/:' '__').tar.gz
  t0=$(date +%s)
  docker save "$img" | gzip -1 >"$b/images/$f"
  id=$(docker image inspect -f '{{.Id}}' "$img")
  digests=$(docker image inspect -f '{{join .RepoDigests ","}}' "$img")
  printf '%s\t%s\t%s\t%s\n' "$img" "$f" "$id" "${digests:-local-build}" >>"$b/images/IMAGES.tsv"
  echo "  $img → images/$f ($(du -h "$b/images/$f" | cut -f1), $(($(date +%s) - t0))s)"
done

if command -v syft >/dev/null; then
  echo "== SBOM (syft)"
  for img in "${IMAGES[@]}"; do
    f=$(echo "$img" | tr '/:' '__').spdx.json
    syft -q "docker:$img" -o "spdx-json=$b/sbom/$f" || echo "  WARN syft failed for $img"
  done
else
  echo "WARN syft not installed: no SBOM in the bundle" | tee "$b/sbom/NO_SBOM.txt"
fi

python3 - "$b" <<'PY'
import json, os, sys, datetime
b = sys.argv[1]
env = dict(l.strip().split("=", 1) for l in open(os.path.join(b, "bundle.env")) if "=" in l)
images = []
for line in open(os.path.join(b, "images", "IMAGES.tsv")):
    ref, f, iid, dg = line.rstrip("\n").split("\t")
    images.append({"ref": ref, "archive": "images/" + f, "imageId": iid,
                   "repoDigests": [] if dg == "local-build" else dg.split(","),
                   "bytes": os.path.getsize(os.path.join(b, "images", f))})
rel = {"format": "sutraerp-onprem-bundle/v1", "version": env["SUTRA_VERSION"], "gitSha": env["SUTRA_GIT_SHA"],
       "builtAt": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
       "capabilities": {"privateCaTrust": env["SUTRA_BUNDLE_PRIVATE_CA_TRUST"] == "true",
                        "documentStorage": env["SUTRA_BUNDLE_DOCUMENTS"] == "true"},
       "images": images, "sbom": sorted(os.listdir(os.path.join(b, "sbom")))}
json.dump(rel, open(os.path.join(b, "RELEASE.json"), "w"), indent=2)
print(f"  RELEASE.json: {len(images)} images, {sum(i['bytes'] for i in images) / 2**20:.0f} MiB compressed")
PY

(cd "$b" && find . -type f ! -name SHA256SUMS -printf '%P\n' | LC_ALL=C sort | xargs -d '\n' sha256sum >SHA256SUMS)
tar -C "$out" -cf "$out/$name.tar" "$name"
sha256sum "$out/$name.tar" | sed "s#$out/##" >"$out/$name.tar.sha256"
echo "bundle $out/$name.tar ($(du -h "$out/$name.tar" | cut -f1)); sha256 $(cut -d' ' -f1 "$out/$name.tar.sha256")"
