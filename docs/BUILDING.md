# Building and deploying FediFetcher-rs

## Requirements

- A recent stable [Rust](https://rustup.rs/) toolchain (Rust 1.99.1+, edition 2024)
- A C and C++ compiler and cmake (for the bundled SQLite and the rustls crypto backend)

## Building from source

```sh
git clone https://github.com/owlrox/fedifetcher-rs.git
cd fedifetcher-rs
cargo build --release
```

The executable is at `target/release/fedifetcher-rs`. To install it onto your `PATH`:

```sh
cargo install --path .
```

Run the test suite with:

```sh
cargo test
```

## Deployment

For a fresh server or LXC container — Alpine, Arch, Debian, or Fedora — `deploy.sh` does the whole setup: it detects the package manager (apk, pacman, apt, or dnf/yum), installs the build dependencies, fetches the source into `/opt/fedifetcher`, installs a Rust toolchain via rustup if the system one is older than the `rust-version` in `Cargo.toml`, builds with `cargo build --release --locked`, installs the binary to `/usr/local/bin/fedifetcher-rs`, creates a `fedifetcher` system user, and sets up a service that runs the web UI (systemd where available, OpenRC on Alpine without systemd).

```sh
sudo ./deploy.sh https://github.com/owlrox/fedifetcher-rs.git [branch]   # branch defaults to main
```

On a fresh box with nothing checked out, run it straight from git — no clone needed:

```sh
curl -fsSL https://raw.githubusercontent.com/owlrox/fedifetcher-rs/main/deploy.sh \
  | sudo sh -s -- https://github.com/owlrox/fedifetcher-rs.git   # branch defaults to main
```

Prefer no build at all? Grab a release binary, untar it, and run it — the tarball holds just the executable:

```sh
curl -fsSL -o /tmp/fedifetcher-rs.tar.gz \
  https://github.com/owlrox/fedifetcher-rs/releases/download/v1.5.4/fedifetcher-rs-x86_64-unknown-linux-musl.tar.gz
tar -xzf /tmp/fedifetcher-rs.tar.gz && sudo install fedifetcher-rs /usr/local/bin/fedifetcher-rs
cd /var/lib/fedifetcher 2>/dev/null || sudo mkdir -p /var/lib/fedifetcher && cd /var/lib/fedifetcher
fedifetcher-rs serve --bind 127.0.0.1 --port 8080
```

The script uses the distribution's own Rust toolchain when it is new enough and falls back to rustup when it is not. Distro Rust versions as of this writing:

| Distribution | Rust shipped | Toolchain used |
| --- | --- | --- |
| Alpine Edge | 1.99.1+ | system |
| Arch | 1.99.1+ | system |
| Fedora Rawhide | 1.99.1+ | system |
| openSUSE Tumbleweed | 1.99.0 | rustup |
| Debian 13 | 1.85.1 | rustup |
| Debian 14 | 1.97.1 | rustup |

Stable Alpine and Fedora releases ship older Rust and get the rustup toolchain. Homebrew on macOS also ships a new-enough Rust for building from source.

The database lands in `/var/lib/fedifetcher/artifacts` — back it up. The web UI listens on `127.0.0.1:8080` by default; override the listen address with `FEDIFETCHER_BIND`/`FEDIFETCHER_PORT`, the branch with `FEDIFETCHER_BRANCH`, and the init system with `FEDIFETCHER_INIT=systemd|openrc|none`. `./deploy.sh --help` lists every override. Re-running the script upgrades in place: it fetches, rebuilds, and reinstalls, and restarts the service only when the binary or its unit changed — a no-change run restarts nothing. After the first run, open `/config` in the web UI to set your server and access tokens, then press **Run**.

To update a server by hand instead:

```sh
cd /opt/fedifetcher
git fetch origin && git reset --hard origin/main
cargo build --release --locked
sudo cp target/release/fedifetcher-rs /usr/local/bin/fedifetcher-rs
sudo systemctl restart fedifetcher    # or: sudo rc-service fedifetcher restart
```

The build needs a C and C++ compiler and cmake, not just a C compiler: the bundled SQLite is compiled in, and rustls's crypto backend (aws-lc-sys) is built with cmake. When running the binary by hand, `cd /var/lib/fedifetcher` first so it finds the same database the service uses. The usual run-only deployment is still the cron job described under [Scheduling regular runs](RUNNING.md#scheduling-regular-runs).

## Development

```sh
cargo test                  # unit and integration tests; the web tests spawn the real server on an ephemeral port
cargo clippy --all-targets  # must pass before committing
cargo fmt --check           # must pass before committing
```

The layout: `src/main.rs` is the command line (`run`/`serve`), `src/lib.rs` and the modules beside it are the library (configuration, database, fetching, the run loop, the web UI), and `tests/` holds the integration tests.
