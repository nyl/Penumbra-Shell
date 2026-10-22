#!/usr/bin/env bash
set -euo pipefail

sysroot=/usr/local/aarch64-sysroot
mkdir -p "$sysroot"

main_url=https://dl-cdn.alpinelinux.org/alpine/edge/main/aarch64
community_url=https://dl-cdn.alpinelinux.org/alpine/edge/community/aarch64
cache=/tmp/penumbra-apk
mkdir -p "$cache"

download_extract() {
    local repo=$1
    local package=$2
    local version=$3
    local archive="$cache/$package-$version.apk"

    if [[ ! -f "$archive" ]]; then
        wget -q "$repo/$package-$version.apk" -O "$archive"
    fi
    tar -xzf "$archive" -C "$sysroot"
}

# These versions match the Alpine repositories used by the Fairphone image.
download_extract "$main_url" eudev-libs 3.2.14-r8
download_extract "$main_url" eudev-dev 3.2.14-r8
download_extract "$main_url" libdrm 2.4.134-r0
download_extract "$main_url" libdrm-dev 2.4.134-r0
download_extract "$main_url" mesa-egl 26.2.5-r0
download_extract "$main_url" mesa-gbm 26.2.5-r0
download_extract "$main_url" mesa-gles 26.2.5-r0
download_extract "$main_url" mesa-dev 26.2.5-r0
download_extract "$community_url" libseat 0.9.3-r1
download_extract "$community_url" libseat-dev 0.9.3-r1
download_extract "$community_url" libinput-libs 1.32.0-r0
download_extract "$community_url" libinput-dev 1.32.0-r0

mkdir -p /usr/local/lib /usr/local/include /usr/local/lib/pkgconfig
cp -a "$sysroot/usr/lib/." /usr/local/lib/
cp -a "$sysroot/usr/include/." /usr/local/include/
cp -a "$sysroot/usr/lib/pkgconfig/." /usr/local/lib/pkgconfig/

printf '%s\n' "Native ARM64 sysroot installed at $sysroot"