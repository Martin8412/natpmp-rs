// CLI help strings are user-facing prose, not API documentation.
#![allow(clippy::doc_markdown)]

mod gateway;
mod natpmp;
mod qbt;

use std::net::Ipv4Addr;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use natpmp::{NatpmpClient, Protocol};

/// NAT-PMP client for ProtonVPN port forwarding with optional qBittorrent integration.
///
/// ProtonVPN gateway: 10.2.0.1
/// If ProtonVPN does not override your default route, also pass --interface <vpn-iface>
/// (e.g. --interface proton0).  No special capabilities needed on Linux >= 5.7.
#[derive(Parser)]
#[command(verbatim_doc_comment)]
struct Cli {
    /// NAT-PMP gateway IP (auto-detected from default route if omitted)
    #[arg(short, long)]
    gateway: Option<Ipv4Addr>,

    /// Network interface to send NAT-PMP requests on (needed when ProtonVPN
    /// does not override the default route)
    #[arg(short, long)]
    interface: Option<String>,

    /// Mapping lifetime in seconds (ProtonVPN uses 60)
    #[arg(long, default_value_t = 60)]
    lifetime: u32,

    /// qBittorrent Web UI URL (omit to skip qBittorrent integration)
    #[arg(long, env = "QBT_URL")]
    qbt_url: Option<String>,

    /// qBittorrent username
    #[arg(long, env = "QBT_USER", default_value = "admin")]
    qbt_user: String,

    /// qBittorrent password
    #[arg(long, env = "QBT_PASS", default_value = "adminadmin")]
    qbt_pass: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let gateway = match cli.gateway {
        Some(gw) => gw,
        None => {
            gateway::default_gateway().context("could not detect default gateway; use --gateway")?
        }
    };
    eprintln!("gateway: {gateway}");

    let client =
        NatpmpClient::new(gateway, cli.interface.as_deref()).context("create NAT-PMP client")?;

    match client.get_public_address() {
        Ok(addr) => eprintln!("public address: {addr}"),
        Err(e) => eprintln!("warning: public address request failed: {e:#}"),
    }

    let qbt = cli
        .qbt_url
        .as_deref()
        .map(|url| qbt::Client::new(url, &cli.qbt_user, &cli.qbt_pass));

    let mut private_port: u16 = 0;

    loop {
        // TCP and UDP both need to be mapped; use the port returned by the
        // first response for the second request.
        let tcp = match client.map_port(private_port, Protocol::Tcp, cli.lifetime) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: TCP port mapping failed: {e:#}; retrying in 30s");
                private_port = 0;
                thread::sleep(Duration::from_secs(30));
                continue;
            }
        };

        let udp = match client.map_port(tcp.private_port, Protocol::Udp, cli.lifetime) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: UDP port mapping failed: {e:#}; retrying in 30s");
                private_port = 0;
                thread::sleep(Duration::from_secs(30));
                continue;
            }
        };

        private_port = tcp.private_port;
        let public_port = tcp.public_port;
        let lifetime = tcp.lifetime.min(udp.lifetime);

        if tcp.public_port != udp.public_port {
            eprintln!(
                "warning: TCP public port {} != UDP public port {}; using TCP port",
                tcp.public_port, udp.public_port
            );
        }

        // Print just the port number to stdout for easy scripting.
        println!("{public_port}");
        eprintln!("mapped: private {private_port} → public {public_port} (lifetime {lifetime}s)");

        if let Some(ref qbt) = qbt {
            match qbt.set_listen_port(public_port) {
                Ok(()) => eprintln!("qBittorrent listen port set to {public_port}"),
                Err(e) => eprintln!("warning: qBittorrent update failed: {e:#}"),
            }
        }

        // Renew well before the mapping expires.
        let renew_in = u64::from(lifetime.saturating_sub(10).max(5));
        eprintln!("renewing in {renew_in}s");
        thread::sleep(Duration::from_secs(renew_in));
    }
}
