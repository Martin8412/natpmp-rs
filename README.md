# natpmp

Keeps a ProtonVPN port-forwarding lease alive and optionally syncs the assigned port to
qBittorrent.

ProtonVPN's P2P servers expose a NAT-PMP gateway at `10.2.0.1`. This tool requests a
forwarded port, renews the lease before it expires, and prints the current public port
to stdout on each renewal. If qBittorrent integration is enabled, qBittorrent's
listening port is updated automatically.

## Requirements

- A ProtonVPN subscription that includes port forwarding (Plus or higher)
- Connected to a ProtonVPN **P2P server** — port forwarding is only available on P2P servers

## Download

Pre-built binaries are on the [Releases](../../releases) page.

| Platform | Binary |
|---|---|
| Linux ARM64 (Raspberry Pi, etc.) | `natpmp-linux-arm64` |
| Linux x86-64 | `natpmp-linux-amd64` |
| macOS Apple Silicon | `natpmp-macos-arm64` |
| macOS Intel | `natpmp-macos-amd64` |
| Windows x86-64 | `natpmp-windows-amd64.exe` |

Make it executable on Linux/macOS:

```
chmod +x natpmp-linux-arm64
```

## Usage

Connect to a ProtonVPN P2P server, then run:

```
natpmp --gateway 10.2.0.1 --interface <vpn-interface>
```

ProtonVPN's NAT-PMP gateway is always `10.2.0.1`. The `--interface` flag routes NAT-PMP
packets through the VPN tunnel — without it the requests go to your home router instead.

To find the VPN interface name:

| OS | Command | Look for |
|---|---|---|
| Linux | `ip link show` | `proton0`, `tun0`, or similar |
| macOS | `ifconfig` | a `utun` interface added when the VPN connected |
| Windows | `ipconfig` | the ProtonVPN adapter, e.g. `ProtonVPN` |

All status messages go to **stderr**; only the bare port number is written to **stdout**
on each renewal. Running the tool interactively you will see both streams:

```
gateway: 10.2.0.1          <- stderr
public address: 185.x.x.x  <- stderr
mapped: private 12345 → public 54321 (lifetime 60s)  <- stderr
54321                       <- stdout
renewing in 50s             <- stderr
```

### Scripting

Use `--port-file` to have the daemon write the current port to a file on every renewal.
Other processes can read the file at any time without having to parse daemon output:

```
natpmp --gateway 10.2.0.1 --interface proton0 --port-file /run/natpmp.port
# elsewhere:
port=$(cat /run/natpmp.port)
```

`--once` maps the port once, prints it to stdout, and exits without starting the renewal
loop. Useful for testing or short-lived connections where you will discard the mapping
before it expires.

### With qBittorrent

```
natpmp --gateway 10.2.0.1 --interface proton0 \
       --qbt-url http://localhost:8080 \
       --qbt-user admin --qbt-pass PASSWORD
```

Credentials can also be supplied via environment variables:

```
export QBT_URL=http://localhost:8080
export QBT_USER=admin
export QBT_PASS=PASSWORD
natpmp --gateway 10.2.0.1 --interface proton0
```

### All options

```
  -g, --gateway <IP>     NAT-PMP gateway IP (auto-detected from default route if omitted)
  -i, --interface <IF>   Network interface for NAT-PMP requests
      --lifetime <SECS>  Mapping lifetime in seconds [default: 60]
      --qbt-url <URL>    qBittorrent Web UI URL [env: QBT_URL]
      --qbt-user <USER>  qBittorrent username [default: admin] [env: QBT_USER]
      --qbt-pass <PASS>  qBittorrent password [default: adminadmin] [env: QBT_PASS]
      --port-file <PATH> Write the current public port to this file on each renewal
      --once             Map once, print the port, and exit
  -h, --help             Print help
```

## Running as a systemd service (Linux)

The repository ships `natpmp@.service` and `natpmp.env` for running the tool as a
persistent background service. The service is a systemd template — the instance name
is the VPN interface, so you can run multiple instances for different interfaces without
editing the unit file.

**1. Set your qBittorrent credentials.**

Edit `natpmp.env`:

```
QBT_URL=http://localhost:8080
QBT_USER=admin
QBT_PASS=your-password
```

**2. Install.**

```
sudo cp natpmp@.service  /etc/systemd/system/
sudo cp natpmp.env       /etc/natpmp.env
sudo chmod 600           /etc/natpmp.env
sudo cp natpmp-linux-arm64 /usr/local/bin/natpmp
sudo systemctl daemon-reload
sudo systemctl enable --now natpmp@proton0
```

Replace `proton0` with your actual VPN interface name. The credentials file is
owned by root (mode 600); systemd reads it before dropping to the `nobody` user.

**3. Check status.**

```
systemctl status natpmp@proton0
journalctl -u natpmp@proton0 -f
```

The service runs as `nobody` and requires no elevated privileges on Linux ≥ 5.7.

## Building from source

```
cargo build --release
```

Cross-compile for all default targets (requires [`cross`](https://github.com/cross-rs/cross)
and Docker for Linux and Windows targets):

```
./build.sh                 # all default targets
./build.sh linux-arm64     # single target
```

See the top of `build.sh` for the full target list and notes on FreeBSD.

## Credits

This project is a Rust implementation of the [NAT-PMP](https://www.rfc-editor.org/rfc/rfc6886)
protocol, based on the original [libnatpmp](http://miniupnp.free.fr/libnatpmp.html) C library
by Thomas Bernard. The translation from C to Rust was done with the assistance of
[Claude](https://claude.ai) (Anthropic).
