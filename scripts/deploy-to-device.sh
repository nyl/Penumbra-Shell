#!/usr/bin/env bash
set -euo pipefail

TARGET="aarch64-unknown-linux-musl"
DEVICE_IP="${DEVICE_IP:-172.16.42.1}"
DEVICE_USER="${DEVICE_USER:-user}"
DEVICE_PASSWORD="${DEVICE_PASSWORD:-}"
SUDO_PASSWORD="${SUDO_PASSWORD:-}"
SESSION_MANAGER="${SESSION_MANAGER:-none}"
BUILD_ONLY=false
DEPLOY_ONLY=false

usage() {
    cat <<'EOF'
Usage: scripts/deploy-to-device.sh [--build-only|--deploy-only] [options]

Options:
  --build-only              Build ARM64 binaries without contacting the device.
  --deploy-only             Deploy the existing target release build.
  --device-ip ADDRESS       Device address (default: 172.16.42.1).
  --device-user USER        SSH user (default: user).
  --device-password PASS    Password for SSH login when key auth is unavailable.
  --sudo-password PASS       Password for sudo on the device when it is not passwordless.
  --session-manager NAME    Configure tinydm or greetd on the device.
  -h, --help                Show this help.
EOF
}

while (($# > 0)); do
    case "$1" in
        --build-only) BUILD_ONLY=true ;;
        --deploy-only) DEPLOY_ONLY=true ;;
        --device-ip)
            (($# >= 2)) || { echo "Missing value for --device-ip" >&2; exit 2; }
            DEVICE_IP=$2
            shift
            ;;
        --device-user)
            (($# >= 2)) || { echo "Missing value for --device-user" >&2; exit 2; }
            DEVICE_USER=$2
            shift
            ;;
        --device-password)
            (($# >= 2)) || { echo "Missing value for --device-password" >&2; exit 2; }
            DEVICE_PASSWORD=$2
            shift
            ;;
        --sudo-password)
            (($# >= 2)) || { echo "Missing value for --sudo-password" >&2; exit 2; }
            SUDO_PASSWORD=$2
            shift
            ;;
        --session-manager)
            (($# >= 2)) || { echo "Missing value for --session-manager" >&2; exit 2; }
            SESSION_MANAGER=$2
            shift
            ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

if [[ "$BUILD_ONLY" == true && "$DEPLOY_ONLY" == true ]]; then
    echo "--build-only and --deploy-only are mutually exclusive" >&2
    exit 2
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
release_dir="$repo_root/target/cross/$TARGET/release"
staging_dir="penumbra-deploy"
remote="${DEVICE_USER}@${DEVICE_IP}"
printf -v remote_env 'SESSION_MANAGER=%q PENUMBRA_LOGIN_USER=%q' "$SESSION_MANAGER" "$DEVICE_USER"

if [[ "$DEPLOY_ONLY" != true ]]; then
    "$repo_root/scripts/build-arm64-alpine.sh"
fi

if [[ "$BUILD_ONLY" == true ]]; then
    exit 0
fi

ssh_base=(ssh -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null)
if [[ -n "$DEVICE_PASSWORD" ]]; then
    if ! command -v sshpass >/dev/null 2>&1; then
        echo "sshpass is required when DEVICE_PASSWORD is set; install it with 'apk add sshpass' or 'apt-get install sshpass'." >&2
        exit 1
    fi
    ssh_base=(sshpass -p "$DEVICE_PASSWORD" ssh -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null)
fi

if ! "${ssh_base[@]}" "$remote" 'true' >/dev/null 2>&1; then
    if [[ -n "$DEVICE_PASSWORD" ]]; then
        echo "SSH authentication using DEVICE_PASSWORD failed. Check the remote password and host reachability." >&2
    else
        echo "SSH authentication failed. Set up key-based auth or pass --device-password/DEVICE_PASSWORD." >&2
    fi
    exit 1
fi

required_bins=(
    penumbra-greeter
    penumbra-comp
    penumbra-top-bar
    penumbra-quicksettings
    penumbra-homescreen
    penumbra-nav-bar
    penumbra-settings
)
for binary in "${required_bins[@]}"; do
    [[ -x "$release_dir/$binary" ]] || {
        echo "Missing release binary: $release_dir/$binary" >&2
        exit 1
    }
done

ssh_base=(ssh -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null)
if [[ -n "$DEVICE_PASSWORD" ]]; then
    ssh_base=(sshpass -p "$DEVICE_PASSWORD" ssh -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null)
fi
scp_base=(scp)
if [[ -n "$DEVICE_PASSWORD" ]]; then
    scp_base=(sshpass -p "$DEVICE_PASSWORD" scp)
fi

"${ssh_base[@]}" "$remote" "mkdir -p '$staging_dir/bin' '$staging_dir/assets'"
"${scp_base[@]}" "${required_bins[@]/#/$release_dir/}" "$remote:$staging_dir/bin/"
"${scp_base[@]}" "$repo_root/scripts/penumbra-session" "$repo_root/scripts/penumbra-nested-session" "$repo_root/scripts/penumbra-native-session" "$repo_root/data/penumbra.desktop" "$repo_root/data/penumbra-settings.desktop" "$remote:$staging_dir/"
if [[ -d "$repo_root/assets" ]]; then
    "${scp_base[@]}" -r "$repo_root/assets/." "$remote:$staging_dir/assets/"
fi

if [[ -z "$SUDO_PASSWORD" ]]; then
    if ! "${ssh_base[@]}" "$remote" 'sudo -n true' >/dev/null 2>&1; then
        read -r -s -p "Device sudo password: " SUDO_PASSWORD
        printf '\n'
    fi
fi

if [[ -n "$SUDO_PASSWORD" ]]; then
    {
        printf '%s\n' "$SUDO_PASSWORD"
        cat <<'REMOTE_INSTALL'
set -euo pipefail

install -d /usr/local/bin /usr/share/wayland-sessions /usr/share/penumbra/assets /usr/share/applications
install -m 0755 penumbra-deploy/bin/* /usr/local/bin/
install -m 0755 penumbra-deploy/penumbra-session /usr/local/bin/penumbra-session
install -m 0755 penumbra-deploy/penumbra-nested-session /usr/local/bin/penumbra-nested-session
install -m 0755 penumbra-deploy/penumbra-native-session /usr/local/bin/penumbra-native-session
install -m 0644 penumbra-deploy/penumbra.desktop /usr/share/wayland-sessions/penumbra.desktop
install -m 0644 penumbra-deploy/penumbra-settings.desktop /usr/share/applications/penumbra-settings.desktop
cp -a penumbra-deploy/assets/. /usr/share/penumbra/assets/

mkdir -p /etc/udev/rules.d
cat > /etc/udev/rules.d/90-backlight.rules <<'UDEV_BACKLIGHT'
ACTION=="add", SUBSYSTEM=="backlight", RUN+="/bin/chgrp video /sys/class/backlight/%k/brightness", RUN+="/bin/chmod 0664 /sys/class/backlight/%k/brightness"
UDEV_BACKLIGHT
udevadm control --reload-rules 2>/dev/null || true
udevadm trigger --subsystem-match=backlight 2>/dev/null || true
chgrp video /sys/class/backlight/*/brightness 2>/dev/null || true
chmod 0664 /sys/class/backlight/*/brightness 2>/dev/null || true
addgroup "${PENUMBRA_LOGIN_USER}" video 2>/dev/null || usermod -aG video "${PENUMBRA_LOGIN_USER}" 2>/dev/null || true
addgroup "${PENUMBRA_LOGIN_USER}" input 2>/dev/null || usermod -aG input "${PENUMBRA_LOGIN_USER}" 2>/dev/null || true
addgroup "${PENUMBRA_LOGIN_USER}" audio 2>/dev/null || usermod -aG audio "${PENUMBRA_LOGIN_USER}" 2>/dev/null || true

if [[ -f /etc/phrog/greetd-config.toml ]]; then
    if [[ ! -f /etc/phrog/greetd-config.toml.penumbra-backup ]]; then
        cp /etc/phrog/greetd-config.toml /etc/phrog/greetd-config.toml.penumbra-backup
    fi
    cat > /etc/phrog/greetd-config.toml <<GREETD_CONFIG
[terminal]
vt = 7

# Start the native Penumbra compositor directly from greetd; avoid the stale phrog/phoc greeter session.
[default_session]
command = "/usr/local/bin/penumbra-native-session"
user = "${PENUMBRA_LOGIN_USER}"
GREETD_CONFIG
elif [[ "${SESSION_MANAGER:-none}" == tiny-dm || "${SESSION_MANAGER:-none}" == tinydm ]]; then
    printf '%s\n' 'penumbra' > /etc/tinydm.conf
elif [[ "${SESSION_MANAGER:-none}" == greetd ]]; then
    install -d /etc/greetd
    cat > /etc/greetd/config.toml <<GREETD_CONFIG
[default_session]
command = "/usr/local/bin/penumbra-native-session"
user = "${PENUMBRA_LOGIN_USER}"
GREETD_CONFIG
elif [[ "${SESSION_MANAGER:-none}" == gdm || "${SESSION_MANAGER:-none}" == none ]]; then
    # GDM / standard display managers use /usr/share/wayland-sessions/penumbra.desktop
    true
else
    echo "No supported session manager configuration found: $SESSION_MANAGER" >&2
    exit 2
fi

rm -rf penumbra-deploy
REMOTE_INSTALL
    } | "${ssh_base[@]}" "$remote" "sudo -S -p '' env $remote_env bash -s"
else
    "${ssh_base[@]}" "$remote" "sudo -n env $remote_env bash -s" <<'REMOTE_INSTALL'
set -euo pipefail

install -d /usr/local/bin /usr/share/wayland-sessions /usr/share/penumbra/assets /usr/share/applications
install -m 0755 penumbra-deploy/bin/* /usr/local/bin/
install -m 0755 penumbra-deploy/penumbra-session /usr/local/bin/penumbra-session
install -m 0755 penumbra-deploy/penumbra-nested-session /usr/local/bin/penumbra-nested-session
install -m 0755 penumbra-deploy/penumbra-native-session /usr/local/bin/penumbra-native-session
install -m 0644 penumbra-deploy/penumbra.desktop /usr/share/wayland-sessions/penumbra.desktop
install -m 0644 penumbra-deploy/penumbra-settings.desktop /usr/share/applications/penumbra-settings.desktop
cp -a penumbra-deploy/assets/. /usr/share/penumbra/assets/

if [[ -f /etc/phrog/greetd-config.toml ]]; then
    if [[ ! -f /etc/phrog/greetd-config.toml.penumbra-backup ]]; then
        cp /etc/phrog/greetd-config.toml /etc/phrog/greetd-config.toml.penumbra-backup
    fi
    cat > /etc/phrog/greetd-config.toml <<GREETD_CONFIG
[terminal]
vt = 7

# Start the native Penumbra compositor directly from greetd; avoid the stale phrog/phoc greeter session.
[default_session]
command = "/usr/local/bin/penumbra-native-session"
user = "${PENUMBRA_LOGIN_USER}"
GREETD_CONFIG
elif [[ "${SESSION_MANAGER:-none}" == tiny-dm || "${SESSION_MANAGER:-none}" == tinydm ]]; then
    printf '%s\n' 'penumbra' > /etc/tinydm.conf
elif [[ "${SESSION_MANAGER:-none}" == greetd ]]; then
    install -d /etc/greetd
    cat > /etc/greetd/config.toml <<GREETD_CONFIG
[default_session]
command = "/usr/local/bin/penumbra-native-session"
user = "${PENUMBRA_LOGIN_USER}"
GREETD_CONFIG
elif [[ "${SESSION_MANAGER:-none}" == gdm || "${SESSION_MANAGER:-none}" == none ]]; then
    # GDM / standard display managers use /usr/share/wayland-sessions/penumbra.desktop
    true
else
    echo "No supported session manager configuration found: $SESSION_MANAGER" >&2
    exit 2
fi

rm -rf penumbra-deploy
REMOTE_INSTALL
fi

missing_libs=$("${ssh_base[@]}" "$remote" 'ldd /usr/local/bin/penumbra-comp 2>&1' | grep -E 'not found|Error loading shared library' || true)
if [[ -n "$missing_libs" ]]; then
    echo "penumbra-comp has unresolved shared libraries on the device:" >&2
    echo "$missing_libs" >&2
    echo "Install the missing packages on the device (Alpine: sudo apk add libseat) and redeploy." >&2
    exit 3
fi

unset SUDO_PASSWORD
unset DEVICE_PASSWORD