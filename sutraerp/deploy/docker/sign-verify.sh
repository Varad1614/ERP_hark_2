#!/usr/bin/env bash
# Sign + SBOM-attest + verify one pushed image by digest (9B, docs/batch9-contracts.md §10.2).
#   sign-verify.sh <registry/repo@sha256:…>
# COSIGN_MODE=keyless (erp_merge / v* tags: GitHub OIDC, Fulcio + Rekor) — verifies against the
#   §10.2 identity: workflow sutraerp-ci.yml on refs/heads/erp_merge or refs/tags/v*.
# COSIGN_MODE=key (branches): a throwaway `cosign generate-key-pair` key (no transparency-log
#   upload) against a local registry — rehearses exactly the same sign → attest → verify path, and
#   proves verification rejects a signature from another key.
# Needs cosign (v2) and syft on PATH. Output: one "ok <check>" line each; non-zero on failure.
set -euo pipefail
REF=${1:?usage: sign-verify.sh <image@sha256:digest>}
MODE=${COSIGN_MODE:-key}
[[ "$REF" == *@sha256:* ]] || { echo "sign by digest only: $REF" >&2; exit 1; }
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
IDENTITY_RE=${COSIGN_IDENTITY_REGEXP:-'^https://github.com/Varad1614/ERP_hark_2/.github/workflows/sutraerp-ci.yml@refs/(heads/erp_merge|tags/v.*)$'}
ISSUER=${COSIGN_OIDC_ISSUER:-https://token.actions.githubusercontent.com}
name=${REF%@*}; name=${name##*/}

case "$REF" in
  localhost:*|127.0.0.1:*) export SYFT_REGISTRY_INSECURE_USE_HTTP=true; INSECURE=(--allow-insecure-registry) ;;
  *) INSECURE=() ;;
esac

# SBOM (SPDX JSON) of the pushed image.
syft scan "registry:$REF" -o "spdx-json=$WORK/sbom.spdx.json" -q
jq -e '.spdxVersion and (.packages | length > 0)' "$WORK/sbom.spdx.json" >/dev/null
echo "ok sbom $name ($(jq '.packages | length' "$WORK/sbom.spdx.json") packages)"
if [[ -n "${SBOM_OUT_DIR:-}" ]]; then mkdir -p "$SBOM_OUT_DIR"; cp "$WORK/sbom.spdx.json" "$SBOM_OUT_DIR/$name.spdx.json"; fi

if [[ "$MODE" == keyless ]]; then
  cosign sign --yes "$REF"
  echo "ok sign $name (keyless)"
  cosign attest --yes --type spdxjson --predicate "$WORK/sbom.spdx.json" "$REF"
  echo "ok attest-sbom $name"
  cosign verify --certificate-identity-regexp "$IDENTITY_RE" --certificate-oidc-issuer "$ISSUER" "$REF" >/dev/null
  echo "ok verify $name"
  cosign verify-attestation --type spdxjson --certificate-identity-regexp "$IDENTITY_RE" --certificate-oidc-issuer "$ISSUER" "$REF" >/dev/null
  echo "ok verify-attestation $name"
  exit 0
fi

# key mode (rehearsal): throwaway key pair, empty password, no Rekor upload.
export COSIGN_PASSWORD=""
( cd "$WORK" && cosign generate-key-pair >/dev/null 2>&1 && mkdir other && cd other && cosign generate-key-pair >/dev/null 2>&1 )
cosign sign --yes --key "$WORK/cosign.key" --tlog-upload=false "${INSECURE[@]}" "$REF"
echo "ok sign $name (throwaway key)"
cosign attest --yes --key "$WORK/cosign.key" --tlog-upload=false --type spdxjson --predicate "$WORK/sbom.spdx.json" "${INSECURE[@]}" "$REF"
echo "ok attest-sbom $name"
cosign verify --key "$WORK/cosign.pub" --insecure-ignore-tlog=true "${INSECURE[@]}" "$REF" >/dev/null
echo "ok verify $name"
cosign verify-attestation --key "$WORK/cosign.pub" --insecure-ignore-tlog=true --type spdxjson "${INSECURE[@]}" "$REF" > "$WORK/att.json"
jq -e -s '.[0].payload | @base64d | fromjson | .predicateType == "https://spdx.dev/Document"' "$WORK/att.json" >/dev/null
echo "ok verify-attestation $name"
if cosign verify --key "$WORK/other/cosign.pub" --insecure-ignore-tlog=true "${INSECURE[@]}" "$REF" >/dev/null 2>&1; then
  echo "verification accepted a signature from the wrong key" >&2; exit 1
fi
echo "ok verify-rejects-wrong-key $name"
