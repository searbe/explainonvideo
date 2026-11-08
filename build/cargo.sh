#!/usr/bin/env bash
# Run cargo in a podman container (build/Containerfile), so the host needs no C toolchain or headers. Usage:
#   build/cargo.sh build --release      build/cargo.sh test
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
img=${EOV_BUILD_IMAGE:-localhost/explainonvideo-build:latest}
if ! podman image exists "$img"; then
  podman build -q -t "$img" -f "$here/build/Containerfile" "$here/build" >/dev/null
fi
cache=${EOV_CARGO_CACHE:-$HOME/.cache/explainonvideo-cargo}
mkdir -p "$here/target" "$cache"
exec podman run --rm --network host --userns keep-id -u "$(id -u):$(id -g)" \
  -v "$here:/work:Z" -v "$cache:/cargo-home:Z" \
  -e CARGO_HOME=/cargo-home -e CARGO_TARGET_DIR=/work/target \
  -w /work "$img" cargo "$@"
