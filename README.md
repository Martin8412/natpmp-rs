# natpmp

Keeps a ProtonVPN port-forwarding lease alive and optionally syncs the assigned port to
qBittorrent.

ProtonVPN's P2P servers expose a NAT-PMP gateway at `10.2.0.1`. This tool requests a
forwarded port, renews the lease every 50 seconds before it expires, and prints the
current public port to stdout on each renewal. If qBittorrent integration is enabled,
qBittorrent's listening port is updated automatically.

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

Example output:

```
gateway: 10.2.0.1
public address: 185.x.x.x
mapped: private 12345 → public 54321 (lifetime 60s)
renewing in 50s
```

The public port is written to **stdout**; all other output goes to **stderr**. This makes
it easy to capture the port in a script:

```
port=$(natpmp --gateway 10.2.0.1 --interface proton0 2>/dev/null | head -1)
```

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
      --qbt-pass <PASS>  qBittorrent password [env: QBT_PASS]
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

Replace `proton0` with your actual VPN interface name.

**4. Check status.**

```
systemctl status natpmp
journalctl -u natpmp -f
```

The service runs as `nobody` and requires no elevated privileges on Linux ≥ 5.7.

## Building from source

```
cargo build --release
```

Cross-compile for all default targets (requires [`cross`](https://github.com/cross-rs/cross)
and Docker for Linux/Windows targets):

```
./build.sh                 # all default targets
./build.sh linux-arm64     # single target
```

See the top of `build.sh` for the full target list and notes on FreeBSD.
