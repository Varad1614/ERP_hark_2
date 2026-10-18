#!/usr/bin/env bash
# Part 2 · S4-docs: builds the MinIO server from its archived upstream source (github.com/minio/minio,
# AGPL-3.0, archived upstream; container images are no longer published or pullable). No third-party
# mirror images are used: the binary comes straight from the upstream module at a pinned release tag
# through the Go module proxy (checksum-verified against sum.golang.org).
#
#   MINIO_TAG=RELEASE.2025-10-15T17-29-55Z OUT=/path/to/minio bash deploy/minio/build-from-source.sh
#
# Needs Go >= 1.24 (the module's go.mod: go 1.24.0, toolchain go1.24.8) and git. Static binary
# (CGO_ENABLED=0). Skips the build when OUT already holds a binary of MINIO_TAG.
set -euo pipefail
MINIO_TAG="${MINIO_TAG:-RELEASE.2025-10-15T17-29-55Z}"
OUT="${OUT:-$PWD/minio}"

if [ -x "$OUT" ] && "$OUT" --version 2>/dev/null | grep -q "${MINIO_TAG#RELEASE.}"; then
  echo "minio $MINIO_TAG already built at $OUT"; "$OUT" --version; exit 0
fi
command -v go >/dev/null || { echo "go is required (>= 1.24)" >&2; exit 1; }
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
# The release tag is not a semantic version; `go install module@<tag>` resolves it to the tagged
# commit. Version metadata like upstream's own build (buildscripts/gen-ldflags.go).
ts="${MINIO_TAG#RELEASE.}"
iso="$(echo "$ts" | sed -E 's/T([0-9]{2})-([0-9]{2})-([0-9]{2})Z/T\1:\2:\3Z/')"
commit="$(git ls-remote https://github.com/minio/minio.git "refs/tags/$MINIO_TAG" | cut -f1)"
[ -n "$commit" ] || { echo "tag $MINIO_TAG not found upstream" >&2; exit 1; }
ldflags="-s -w -X github.com/minio/minio/cmd.Version=$iso -X github.com/minio/minio/cmd.ReleaseTag=$MINIO_TAG -X github.com/minio/minio/cmd.CommitID=$commit -X github.com/minio/minio/cmd.ShortCommitID=${commit:0:12}"
echo "building minio $MINIO_TAG ($commit) …"
GOBIN="$work/bin" CGO_ENABLED=0 GOFLAGS=-trimpath go install -ldflags "$ldflags" "github.com/minio/minio@$MINIO_TAG"
mkdir -p "$(dirname "$OUT")"
install -m 0755 "$work/bin/minio" "$OUT"
"$OUT" --version
