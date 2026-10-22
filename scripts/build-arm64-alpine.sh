#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"
image=${PENUMBRA_ARM64_IMAGE:-penumbra/aarch64-alpine:edge}
docker build --tag "$image" --file "$repo_root/Dockerfile.aarch64-alpine" "$repo_root"

docker run --rm \
    --volume "$repo_root:/workspace" \
    --volume "$repo_root/target/cross:/workspace/target" \
    --workdir /workspace \
    "$image" \
    sh -eu -c '
        cargo zigbuild --workspace --exclude penumbra-comp --target aarch64-unknown-linux-musl --release
        cargo zigbuild --target aarch64-unknown-linux-musl --release -p penumbra-comp --features native-drm
    '