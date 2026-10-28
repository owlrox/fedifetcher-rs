# FediFetcher-rs

[![License: GPL-3.0-only](https://img.shields.io/badge/license-GPL--3.0--only-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/language-Rust-orange)](https://www.rust-lang.org/)

Pull missing posts and profiles into your Mastodon instance: the context of what your users reply to, boost, bookmark, favorite, list, and get notified about, fetched from the servers that host it.

FediFetcher-rs is a Rust port of the Python [FediFetcher](https://github.com/nanos/FediFetcher) (v8.2.3), with a server-rendered web UI that needs no JavaScript.

This fork exists purely to make FediFetcher easier to run. The Python original wants a Docker image to configure, environment variables to spell correctly, API keys to create and paste by hand, and Python dependencies to keep updated; this port does away with all of that. It builds to a single binary with no runtime dependencies, the web UI walks you through connecting your account (no manual key wrangling), and the whole configuration lives in one SQLite database you can back up.

## Quick start

Build from source (needs Rust 1.99.1+ plus a C/C++ compiler and cmake — see [docs/BUILDING.md](docs/BUILDING.md)):

```sh
git clone https://github.com/owlrox/fedifetcher-rs.git
cd fedifetcher-rs
cargo build --release
```

The executable is at `target/release/fedifetcher-rs`; `cargo install --path .` puts it on your `PATH`.

On a fresh server or LXC container, let `deploy.sh` do the whole setup — package manager detection, build dependencies, binary install, systemd/OpenRC service:

```sh
curl -fsSL https://raw.githubusercontent.com/owlrox/fedifetcher-rs/main/deploy.sh \
  | sudo sh -s -- https://github.com/owlrox/fedifetcher-rs.git
```

Prefer no build at all? Grab a release binary, untar it, and run it (other platforms in [docs/BUILDING.md](docs/BUILDING.md#deployment)):

```sh
curl -fsSL -o /tmp/fedifetcher-rs.tar.gz \
  https://github.com/owlrox/fedifetcher-rs/releases/download/v1.5.4/fedifetcher-rs-x86_64-unknown-linux-musl.tar.gz
tar -xzf /tmp/fedifetcher-rs.tar.gz && sudo install fedifetcher-rs /usr/local/bin/fedifetcher-rs
```

Then open the web UI at <http://127.0.0.1:8080> and press **Run** (the [docs](docs/RUNNING.md) cover the run modes, the config page, scheduling, and service setup).

## Documentation

- [docs/BUILDING.md](docs/BUILDING.md) — requirements, building from source, testing, and deploying with `deploy.sh`
- [docs/RUNNING.md](docs/RUNNING.md) — run modes (`run`, `serve`, `schedule`), the web UI, scheduling regular runs, every configuration option with its default, and the state directory
- [docs/ROADMAP.md](docs/ROADMAP.md) — ideas for future releases, with what probing a live server confirmed
- [CHANGELOG.md](CHANGELOG.md) — versions and their write-ups, newest first

## License

FediFetcher-rs is free software: you can redistribute it and/or modify it under the terms of the [GNU General Public License](https://www.gnu.org/licenses/gpl-3.0.html) as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. The full text ships as `LICENSE`.

FediFetcher-rs itself is Copyright (C) 2026 owlrox — [github.com/owlrox/fedifetcher-rs](https://github.com/owlrox/fedifetcher-rs).

It is a Rust port of the original [FediFetcher](https://github.com/nanos/FediFetcher), Copyright (C) Michael Thomas, which is MIT-licensed; the upstream copyright and MIT notice are kept in `NOTICE.md`, as the MIT license requires.
