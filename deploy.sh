#!/bin/sh
# deploy.sh - fetch, build, install, and run FediFetcher on a fresh LXC/VM.
#
# Usage:
#   sudo ./deploy.sh [REPO_URL] [BRANCH]
#
# What it does:
#   1. Detects the package manager (Alpine/apk, Debian/apt, Fedora/dnf or yum)
#      and installs git, a C/C++ toolchain, and cmake. The C++ compiler and
#      cmake are needed by aws-lc-sys (rustls's crypto backend), not just the
#      C compiler the README mentions for the bundled SQLite.
#   2. Clones the repo into $INSTALL_DIR, or git fetch + reset if it is
#      already a checkout, so re-running the script upgrades in place.
#      Re-running is idempotent: packages, user, state, and service files
#      converge, and the running service is only restarted when the binary
#      or its unit actually changed.
#   3. Installs a Rust toolchain via rustup if the system one is missing or
#      older than the rust-version in Cargo.toml (distro Rust is usually too
#      old for edition 2024).
#   4. Builds with cargo build --release --locked and installs the binary
#      to $BIN_DIR/fedifetcher.
#   5. Creates a system user, a state directory, and a service that runs
#      `fedifetcher serve`: systemd if available, OpenRC on Alpine LXC
#      without systemd, otherwise manual run instructions.
#
# Configuration (server, access tokens) happens in the web UI's /config page
# after the service is up; there is no config file.
#
# Environment overrides:
#   FEDIFETCHER_REPO_URL   default repo URL (argument 1 wins)
#   FEDIFETCHER_BRANCH     default branch (argument 2 wins, default: main)
#   INSTALL_DIR            source checkout       (default /opt/fedifetcher)
#   BIN_DIR                binary destination    (default /usr/local/bin)
#   STATE_DIR              database/lock dir     (default /var/lib/fedifetcher)
#   FEDIFETCHER_BIND/PORT  web UI listen address (default 127.0.0.1:8080)
#   FEDIFETCHER_INIT       auto|systemd|openrc|none (default: auto)

set -eu
umask 022

REPO_URL=${1:-${FEDIFETCHER_REPO_URL:-}}
BRANCH=${2:-${FEDIFETCHER_BRANCH:-main}}
INSTALL_DIR=${INSTALL_DIR:-/opt/fedifetcher}
BIN_DIR=${BIN_DIR:-/usr/local/bin}
STATE_DIR=${STATE_DIR:-/var/lib/fedifetcher}
BIND=${FEDIFETCHER_BIND:-127.0.0.1}
PORT=${FEDIFETCHER_PORT:-8080}
INIT=${FEDIFETCHER_INIT:-auto}
SYSTEMD_DIR=${SYSTEMD_DIR:-/etc/systemd/system}
INITD_DIR=${INITD_DIR:-/etc/init.d}
SERVICE=fedifetcher
USER=fedifetcher

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
fi

log()  { printf '[deploy] %s\n' "$*"; }
step() { printf '\n[deploy] === %s ===\n' "$*"; }
die()  { printf '[deploy] ERROR: %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "must run as root: sudo ./deploy.sh [REPO_URL] [BRANCH]"

# --- 1. Package manager and build dependencies ------------------------------

PKG=
if command -v apk >/dev/null 2>&1; then
    PKG=apk
elif command -v apt-get >/dev/null 2>&1; then
    PKG=apt
elif command -v dnf >/dev/null 2>&1; then
    PKG=dnf
elif command -v yum >/dev/null 2>&1; then
    PKG=yum
else
    die "no supported package manager found (need apk, apt-get, dnf, or yum)"
fi
OS_NAME=$(sed -n 's/^NAME="\?\([^"]*\)"\?/\1/p' /etc/os-release 2>/dev/null || true)
log "detected package manager: $PKG${OS_NAME:+ ($OS_NAME)}"

step "installing build dependencies ($PKG)"
case $PKG in
    apk)
        apk add --no-cache git gcc g++ musl-dev make cmake perl curl
        ;;
    apt)
        apt-get update
        DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
            git build-essential cmake curl
        ;;
    dnf)
        dnf install -y git gcc gcc-c++ make cmake curl
        ;;
    yum)
        yum install -y git gcc gcc-c++ make cmake curl
        ;;
esac
command -v git >/dev/null 2>&1 || die "git is not installed"

# --- 2. Source: clone, or fetch + reset an existing checkout ----------------

step "fetching source into $INSTALL_DIR (branch: $BRANCH)"
if [ -d "$INSTALL_DIR/.git" ]; then
    if ! git -C "$INSTALL_DIR" remote get-url origin >/dev/null 2>&1; then
        [ -n "$REPO_URL" ] || die "$INSTALL_DIR is a checkout without an origin remote; pass REPO_URL"
        git -C "$INSTALL_DIR" remote add origin "$REPO_URL"
    fi
    git -C "$INSTALL_DIR" fetch --prune origin
    git -C "$INSTALL_DIR" reset --hard "origin/$BRANCH"
    log "updated existing checkout to origin/$BRANCH"
else
    [ -n "$REPO_URL" ] || die "no checkout at $INSTALL_DIR; pass REPO_URL as the first argument"
    mkdir -p "$INSTALL_DIR"
    git clone --branch "$BRANCH" "$REPO_URL" "$INSTALL_DIR"
    log "cloned $REPO_URL ($BRANCH)"
fi

# --- 3. Rust toolchain -------------------------------------------------------

# version_ge INSTALLED REQUIRED: exit 0 if INSTALLED >= REQUIRED (dotted ints)
version_ge() {
    awk -v a="$1" -v b="$2" 'BEGIN {
        n = split(a, x, "."); m = split(b, y, ".")
        for (i = 1; i <= (n > m ? n : m); i++) {
            xi = (i in x) ? x[i] + 0 : 0
            yi = (i in y) ? y[i] + 0 : 0
            if (xi > yi) exit 0
            if (xi < yi) exit 1
        }
        exit 0
    }'
}

rustc_version() {
    rustc --version 2>/dev/null | awk '{print $2}' | sed 's/[^0-9.].*//'
}

REQUIRED=$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([^"]*\)"/\1/p' \
    "$INSTALL_DIR/Cargo.toml" | head -n 1)
REQUIRED=${REQUIRED:-1.85}  # edition 2024 floor if Cargo.toml says nothing

step "checking Rust toolchain (need >= $REQUIRED)"
CARGO_BIN=${CARGO_HOME:-$HOME/.cargo}/bin
# A rustup toolchain from a previous run may not be on PATH in a non-login
# shell; pick it up before deciding to install again.
if [ -x "$CARGO_BIN/rustc" ] && [ -x "$CARGO_BIN/cargo" ]; then
    PATH="$CARGO_BIN:$PATH"
    export PATH
fi
if command -v rustc >/dev/null 2>&1 && version_ge "$(rustc_version)" "$REQUIRED"; then
    log "system Rust $(rustc_version) is new enough"
else
    log "installing Rust via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init.sh
    sh /tmp/rustup-init.sh -y --default-toolchain stable
    rm -f /tmp/rustup-init.sh
    PATH="$CARGO_BIN:$PATH"
    export PATH
    command -v cargo >/dev/null 2>&1 || die "rustup did not install cargo"
fi
log "using $(command -v cargo) ($(rustc --version))"

# --- 4. Build and install ----------------------------------------------------

step "building (cargo build --release --locked)"
( cd "$INSTALL_DIR" && cargo build --release --locked )
[ -x "$INSTALL_DIR/target/release/$SERVICE" ] \
    || die "build did not produce target/release/$SERVICE"

step "installing binary to $BIN_DIR/$SERVICE"
mkdir -p "$BIN_DIR"
if [ -f "$BIN_DIR/$SERVICE" ] && cmp -s "$INSTALL_DIR/target/release/$SERVICE" "$BIN_DIR/$SERVICE"; then
    CHANGED=0
else
    CHANGED=1
fi
cp "$INSTALL_DIR/target/release/$SERVICE" "$BIN_DIR/$SERVICE"
chmod 0755 "$BIN_DIR/$SERVICE"
"$BIN_DIR/$SERVICE" --version

# --- 5. User and state directory ---------------------------------------------

step "creating user $USER and state dir $STATE_DIR"
if ! id "$USER" >/dev/null 2>&1; then
    NOLOGIN=/usr/sbin/nologin
    [ -x "$NOLOGIN" ] || NOLOGIN=/sbin/nologin
    if command -v useradd >/dev/null 2>&1; then
        groupadd --system "$USER" 2>/dev/null || true
        useradd --system --gid "$USER" --home-dir "$STATE_DIR" \
            --shell "$NOLOGIN" --no-create-home "$USER"
    else
        addgroup -S "$USER"
        adduser -S -D -H -h "$STATE_DIR" -s "$NOLOGIN" -G "$USER" "$USER"
    fi
fi
# The binary stores its database in ./artifacts when that directory exists in
# the working directory, so the service's WorkingDirectory plus this
# artifacts/ dir keeps everything under $STATE_DIR.
mkdir -p "$STATE_DIR/artifacts"
chown -R "$USER:$USER" "$STATE_DIR"
chmod 0750 "$STATE_DIR" "$STATE_DIR/artifacts"

# --- 6. Service ---------------------------------------------------------------

if [ "$INIT" = auto ]; then
    if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
        INIT=systemd
    elif command -v rc-service >/dev/null 2>&1; then
        INIT=openrc
    else
        INIT=none
    fi
fi

case $INIT in
systemd)
    step "installing systemd service"
    mkdir -p "$SYSTEMD_DIR"
    UNIT_TMP="$SYSTEMD_DIR/$SERVICE.service.tmp"
    cat > "$UNIT_TMP" <<EOF
[Unit]
Description=FediFetcher - pull missing posts and profiles into your Mastodon instance
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$USER
Group=$USER
WorkingDirectory=$STATE_DIR
ExecStart=$BIN_DIR/$SERVICE serve --bind $BIND --port $PORT
Restart=on-failure
RestartSec=10s

[Install]
WantedBy=multi-user.target
EOF
    if [ -f "$SYSTEMD_DIR/$SERVICE.service" ] && cmp -s "$UNIT_TMP" "$SYSTEMD_DIR/$SERVICE.service"; then
        UNIT_CHANGED=0
        rm -f "$UNIT_TMP"
    else
        UNIT_CHANGED=1
        mv "$UNIT_TMP" "$SYSTEMD_DIR/$SERVICE.service"
    fi
    systemctl daemon-reload
    systemctl enable "$SERVICE.service"
    if [ "$CHANGED" = 1 ] || [ "$UNIT_CHANGED" = 1 ] || ! systemctl is-active --quiet "$SERVICE.service"; then
        systemctl restart "$SERVICE.service"
        log "service (re)started"
    else
        log "binary and unit unchanged - service left running"
    fi
    if ! systemctl is-active --quiet "$SERVICE.service"; then
        log "WARNING: $SERVICE is not active - check: systemctl status $SERVICE"
    fi
    ;;
openrc)
    step "installing OpenRC service"
    mkdir -p "$INITD_DIR"
    INIT_TMP="$INITD_DIR/$SERVICE.tmp"
    cat > "$INIT_TMP" <<EOF
#!/sbin/openrc-run

name="$SERVICE"
description="FediFetcher - pull missing posts and profiles into your Mastodon instance"

command="$BIN_DIR/$SERVICE"
command_args="serve --bind $BIND --port $PORT"
command_user="$USER:$USER"
directory="$STATE_DIR"
pidfile="/run/$SERVICE.pid"
output_log="/var/log/$SERVICE.log"
error_log="/var/log/$SERVICE.log"
supervisor=supervise-daemon

depend() {
    need net
    after firewall
}
EOF
    chmod 0755 "$INIT_TMP"
    if [ -f "$INITD_DIR/$SERVICE" ] && cmp -s "$INIT_TMP" "$INITD_DIR/$SERVICE"; then
        INIT_CHANGED=0
        rm -f "$INIT_TMP"
    else
        INIT_CHANGED=1
        mv "$INIT_TMP" "$INITD_DIR/$SERVICE"
    fi
    rc-update add "$SERVICE" default
    if [ "$CHANGED" = 1 ] || [ "$INIT_CHANGED" = 1 ] || ! rc-service "$SERVICE" status >/dev/null 2>&1; then
        rc-service "$SERVICE" restart
        log "service (re)started"
    else
        log "binary and init script unchanged - service left running"
    fi
    ;;
none)
    step "no init system found - skipping service setup"
    ;;
*)
    die "unknown FEDIFETCHER_INIT: $INIT (want auto, systemd, openrc, or none)"
    ;;
esac

# --- Done ---------------------------------------------------------------------

step "done"
cat <<EOF

FediFetcher is installed.

  Binary:    $BIN_DIR/$SERVICE
  State dir: $STATE_DIR/artifacts  (fedifetcher.db lives here - back it up)
  Web UI:    http://$BIND:$PORT/    (open /config to set your server and
             access tokens, then press Run on the dashboard)
EOF
case $INIT in
systemd) log "manage with: systemctl status|stop|start|restart $SERVICE" ;;
openrc)  log "manage with: rc-service $SERVICE status|stop|start|restart" ;;
none)
    cat <<EOF

No init system was detected, so nothing was started. To run the web UI in
the foreground as $USER:

  su -s /bin/sh -c 'cd $STATE_DIR && $BIN_DIR/$SERVICE serve --bind $BIND --port $PORT' $USER

Or run one pass, e.g. from $USER's crontab:

  0 * * * * cd $STATE_DIR && $BIN_DIR/$SERVICE run
EOF
    ;;
esac
log "re-run this script any time to fetch, rebuild, and upgrade in place"
