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
# Parsing options are kept as an array, and as individual values for the build
# args. A delimiter can legitimately be a glob character or whitespace, and a
# single flat string would be word-split and pathname-expanded at every use.
FLAGS=()
DELIMITER=""
HEADER=0
ALLOW_BINARY=0

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
    --allow-binary)  ALLOW_BINARY=1; FLAGS+=(--allow-binary); shift ;;
    --header)        HEADER=1; FLAGS+=(--header); shift ;;
    --delimiter)     DELIMITER="$2"; FLAGS+=(--delimiter "$2"); shift 2 ;;
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
  # The ${FLAGS[@]+...} guard keeps an empty array from tripping `set -u` on
  # bash releases before 4.4, which macOS still ships.
  cargo run --quiet --release -- check "$DATA" ${FLAGS[@]+"${FLAGS[@]}"}
else
  echo "    cargo not found; skipping local validation (the build stage still validates)"
fi

# The dataset must be inside the build context for COPY to reach it, and every
# input is staged to a generated name at the context root -- including one that
# already lives in the repository. Passing the user's own path through was
# wrong three separate ways:
#
#   - `.dockerignore` excludes bench, docs and target, so a dataset under any
#     of them is absent from the context. `pack.sh --data bench/kv.tsv` (the
#     path this repo's own benchmark generates) passed local validation and
#     then died at COPY with "not found".
#   - COPY treats its source as a Go filepath.Match pattern, so a name holding
#     `*`, `?` or `[...]` may match something other than itself -- or match
#     several files, after which `find | head -1` compiles whichever came
#     first.
#   - Two runs packaging different files that share a basename collided on one
#     staged path.
#
# The staged name is therefore built here, not taken from input: a fixed stem,
# the pid for uniqueness, and only the extension carried over, because
# delimiter inference reads it. The extension is stripped of anything that is
# not alphanumeric so it cannot reintroduce a wildcard.
base=$(basename "$DATA")
ext=""
case "$base" in
  ?*.*) ext=$(printf '%s' "${base##*.}" | tr -cd '[:alnum:]') ;;
esac
ctx_data=".packdata.$$.data${ext:+.$ext}"
cp "$DATA" "$repo_root/$ctx_data"
cleanup="$repo_root/$ctx_data"
trap '[[ -n "$cleanup" ]] && rm -f "$cleanup"' EXIT

echo "==> building $TAG for $PLATFORM from $ctx_data"
# Each parsing option travels as its own build arg. Collapsing them into one
# string would hand the delimiter back to word splitting inside the image.
docker buildx build \
  --platform "$PLATFORM" \
  --build-arg "DATA=$ctx_data" \
  --build-arg "DELIMITER=$DELIMITER" \
  --build-arg "HEADER=$HEADER" \
  --build-arg "ALLOW_BINARY=$ALLOW_BINARY" \
  --tag "$TAG" \
  --load \
  .

echo "==> done: $TAG"
echo "    run: docker run --rm -p 8080:8080 $TAG"
