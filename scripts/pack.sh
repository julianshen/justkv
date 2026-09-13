#!/usr/bin/env bash
# Validate a dataset, then build a container image containing the server and
# the compiled data.
#
# Validation runs locally first so a bad dataset fails in seconds instead of
# after a full image build.
set -euo pipefail

DATA=""
TAG="justkv:latest"
PLATFORM="linux/amd64"
CHECK_FLAGS=""

usage() {
  cat <<'USAGE'
Usage: scripts/pack.sh --data <file> [--tag <name:tag>] [--platform <p>] [--allow-binary] [--header] [--delimiter <c>]

  --data       CSV/TSV dataset to bake into the image (required)
  --tag        image tag                  (default: justkv:latest)
  --platform   target platform            (default: linux/amd64)
  --allow-binary, --header, --delimiter   passed through to check/build
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --data)          DATA="$2"; shift 2 ;;
    --tag)           TAG="$2"; shift 2 ;;
    --platform)      PLATFORM="$2"; shift 2 ;;
    --allow-binary)  CHECK_FLAGS="$CHECK_FLAGS --allow-binary"; shift ;;
    --header)        CHECK_FLAGS="$CHECK_FLAGS --header"; shift ;;
    --delimiter)     CHECK_FLAGS="$CHECK_FLAGS --delimiter $2"; shift 2 ;;
    -h|--help)       usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage; exit 2 ;;
  esac
done

if [[ -z "$DATA" ]]; then
  echo "error: --data is required" >&2
  usage
  exit 2
fi
if [[ ! -f "$DATA" ]]; then
  echo "error: no such file: $DATA" >&2
  exit 2
fi

# Resolve now, before the cd below changes what a relative path means.
DATA="$(realpath "$DATA")"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Fast local validation: seconds of feedback instead of a full image build.
echo "==> validating $DATA"
if command -v cargo >/dev/null 2>&1; then
  # shellcheck disable=SC2086
  cargo run --quiet --release -- check "$DATA" $CHECK_FLAGS
else
  echo "    cargo not found; skipping local validation (the build stage still validates)"
fi

# The dataset must be inside the build context for COPY to reach it.
# Note: GNU `realpath --relative-to` is not available on macOS/BSD realpath,
# so we resolve the absolute path once and strip the repo-root prefix with a
# portable shell parameter expansion instead.
ctx_data="$DATA"
cleanup=""
# $DATA was already resolved to an absolute path above, before the cd.
abs_data="$DATA"
case "$abs_data" in
  "$repo_root"/*) ctx_data="${abs_data#"$repo_root"/}" ;;
  *)
    # Keep the original basename: delimiter inference reads the extension, so
    # copying a .tsv to a fixed name would silently make it comma-delimited
    # inside the image — local validation passes, then the build fails with a
    # confusing "expected 2 columns". The leading dot keeps it out of the way.
    ctx_data=".packdata.$(basename "$DATA")"
    cp "$DATA" "$repo_root/$ctx_data"
    cleanup="$repo_root/$ctx_data"
    ;;
esac
trap '[[ -n "$cleanup" ]] && rm -f "$cleanup"' EXIT

echo "==> building $TAG for $PLATFORM from $ctx_data"
docker buildx build \
  --platform "$PLATFORM" \
  --build-arg "DATA=$ctx_data" \
  --build-arg "CHECK_FLAGS=$CHECK_FLAGS" \
  --tag "$TAG" \
  --load \
  .

echo "==> done: $TAG"
echo "    run: docker run --rm -p 8080:8080 $TAG"
