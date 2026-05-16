# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                  # debug build
cargo build --release        # release build
cargo clippy                 # lint
cargo test                   # run tests (none yet)
cargo run -- --help          # show CLI help
```

## Cross-compilation

`build.sh` cross-compiles for all targets and places stripped binaries in `dist/`.
Linux targets use `cross` (Docker required); macOS targets use `cargo` directly.

```bash
./build.sh                   # all four targets
./build.sh linux-arm64       # primary target — aarch64 Linux, static musl binary
./build.sh linux-amd64 macos-arm64 macos-amd64
```

Linux targets (`linux-arm64`, `linux-amd64`) produce fully static musl binaries.
macOS targets (`macos-arm64`, `macos-amd64`) are built natively via Xcode toolchain.

## Architecture

Single binary (`natpmp`) with four modules:

- **`src/natpmp.rs`** — NAT-PMP protocol over UDP. `NatpmpClient::new(gateway, interface)` creates a connected UDP socket. `map_port()` and `get_public_address()` implement the request/retry loop: sends a request then waits with exponential back-off (250 ms → 500 ms → … → 64 s, 9 attempts total, matching RFC 6886 §3.1). Interface binding is platform-specific: `SO_BINDTODEVICE` on Linux, `IP_BOUND_IF` on macOS.

- **`src/gateway.rs`** — Default gateway detection: parses `/proc/net/route` on Linux, shells out to `route -n get default` on macOS.

- **`src/qbt.rs`** — qBittorrent Web API client. `Client::set_listen_port(port)` logs in (`POST /api/v2/auth/login`), captures the `SID` cookie, then calls `POST /api/v2/app/setPreferences` with `json={"listen_port":<port>}`.

- **`src/main.rs`** — CLI (`clap` derive), main renewal loop. Starts with `private_port = 0` so ProtonVPN assigns the port; stores the returned private port and reuses it on every renewal. Prints the public port to **stdout** and all status messages to **stderr** (useful for scripting).

## ProtonVPN specifics

ProtonVPN's NAT-PMP gateway is `10.2.0.1`. On this machine ProtonVPN does **not** override the default route, so `--interface <vpn-iface>` is required to route UDP packets through the VPN. `SO_BINDTODEVICE` on an unbound socket requires no special capability on Linux ≥ 5.7; the binary runs fine as an unprivileged user.

Typical invocation:
```bash
natpmp --gateway 10.2.0.1 --interface proton0 \
       --qbt-url http://localhost:8080 --qbt-user admin --qbt-pass PASSWORD
```

The mapping lifetime is 60 s; the loop renews 10 s before expiry (every 50 s by default).

## systemd service

`natpmp.service` and `natpmp.env` are included in the repo root. The service uses `BindsTo=sys-subsystem-net-devices-proton0.device` so it starts when the VPN interface appears and restarts when it drops. Credentials live in `/etc/natpmp.env` (mode 600). The binary runs as `nobody` with no special capabilities required (Linux ≥ 5.7).
