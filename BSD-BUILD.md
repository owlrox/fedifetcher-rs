# Building fedifetcher-rs on the BSDs

Status: expected to build on FreeBSD, OpenBSD, and NetBSD. The Rust code
typechecks clean for `x86_64-unknown-freebsd`; the only thing a macOS
cross-check could not finish is the C crypto backend (`aws-lc-sys`), which
needs a real BSD toolchain. A native build on a BSD host is the definitive
test.

## Why it should work

- Only platform gates in `src/` are `cfg(unix)` (`store.rs:236`, `web.rs:156`,
  `store/tests.rs:194`, `store/tests.rs:224`) — BSD counts as unix, so those
  paths compile.
- `src/mem.rs` reads memory metrics on Linux and macOS only and falls back to
  `None` elsewhere. On BSD the dashboard's RSS/heap columns will be blank;
  nothing fails.
- Dependencies are portable:
  - `reqwest` uses rustls — no system OpenSSL needed.
  - `sqlx` bundles SQLite (C, builds with plain `cc`).
  - Everything else is pure Rust or C that compiles with an unmodified
    `cc`.
- Known cross-compile blocker from macOS/Linux: `aws-lc-sys` (rustls' default
  crypto backend) must compile C, which needs the target's headers and
  linker. A native BSD build avoids this entirely.

## FreeBSD (the main target)

```sh
pkg install rust sqlite3   # sqlite3 optional; sqlx bundles its own
git clone https://github.com/owlrox/fedifetcher-rs
cd fedifetcher-rs
cargo build --release
cargo test
./target/release/fedifetcher-rs --help
```

Requires Rust 1.99.1+ (`pkg` ships a current rust; `rustup` also works).

## OpenBSD

```sh
pkg_add rust
git clone https://github.com/owlrox/fedifetcher-rs
cd fedifetcher-rs
cargo build --release
cargo test
```

OpenBSD's unveil/pledge do not interfere with an ordinary Rust binary, but if
you sandbox it, allow network out and rw on the data dir.

## NetBSD

```sh
pkgin install rust
git clone https://github.com/owlrox/fedifetcher-rs
cd fedifetcher-rs
cargo build --release
cargo test
```

## Cross-compiling from an x86 Linux PC instead

If a BSD VM is inconvenient, Docker-based cross builds work on any x86_64
Linux host:

```sh
cargo install cross
cross build --release --target x86_64-unknown-freebsd
```

`cross` ships a FreeBSD sysroot image for this target. OpenBSD/NetBSD targets
are less polished in `cross`; a native build or VM is more reliable there.

## On an Apple Silicon Mac (what was already tried)

```sh
rustup target add x86_64-unknown-freebsd
cargo check --target x86_64-unknown-freebsd --all-targets
```

Note: Homebrew's rustc shadows rustup's on PATH, so run
`PATH="$HOME/.cargo/bin:$PATH" cargo check ...` or the target is not found.
The check gets as far as `aws-lc-sys` and stops — expected, since compiling
its C needs a FreeBSD sysroot the Mac does not have. Everything before that
(rust and all pure-Rust deps) passes.

## Known gap: memory metrics on BSD

`src/mem.rs` reports RSS/peak RSS/heap on Linux and macOS only. On BSD the
values fall back to `None` and the dashboard shows blanks. Both FreeBSD and
OpenBSD expose `getrusage(2)` (via the existing `libc` dependency, which is
already gated `cfg(unix)`), so adding a
`#[cfg(target_os = "freebsd")]` / `#[cfg(target_family = "unix")]` branch is
a small follow-up. Watch the unit: `ru_maxrss` is in kilobytes on Linux,
bytes on FreeBSD.

## Report back

If a build fails on any of the three, capture:

```sh
cargo build --release 2>&1 | tail -50
rustc --version
uname -a
```

The likely failure points are `aws-lc-sys` (needs a recent `cc`; if the
distro's toolchain is old, try `rustls` with the `ring` provider instead) and
the bundled SQLite in `libsqlite3-sys` (needs a working `cc`).
