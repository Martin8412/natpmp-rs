use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

use anyhow::{Context, Result, bail};

const NATPMP_PORT: u16 = 5351;
// RFC 6886 §3.1: 9 total attempts, timeouts 250→500→…→64000 ms (≈127.75 s total)
const MAX_ATTEMPTS: usize = 9;

#[derive(Debug, Clone, Copy)]
pub enum Protocol {
    Udp,
    Tcp,
}

#[derive(Debug)]
pub struct PortMapping {
    pub private_port: u16,
    pub public_port: u16,
    pub lifetime: u32,
}

pub struct NatpmpClient {
    socket: UdpSocket,
    gateway: Ipv4Addr,
}

impl NatpmpClient {
    /// # Errors
    /// Returns an error if the UDP socket cannot be bound or if binding to the
    /// specified interface fails.
    pub fn new(gateway: Ipv4Addr, interface: Option<&str>) -> Result<Self> {
        let socket = make_socket(interface)?;
        socket
            .connect((gateway, NATPMP_PORT))
            .context("connect to NAT-PMP gateway")?;
        Ok(Self { socket, gateway })
    }

    /// Alternative constructor that connects to `gateway:port` instead of the
    /// standard port 5351.  Useful for integration tests that spin up a mock
    /// server on an OS-assigned loopback port.
    ///
    /// # Errors
    /// Returns an error if the UDP socket cannot be bound or if binding to the
    /// specified interface fails.
    #[allow(dead_code)]
    pub fn new_with_port(gateway: Ipv4Addr, port: u16, interface: Option<&str>) -> Result<Self> {
        let socket = make_socket(interface)?;
        socket
            .connect((gateway, port))
            .context("connect to NAT-PMP gateway")?;
        Ok(Self { socket, gateway })
    }

    // RFC 6886 §3.1: send once, then retransmit only on timeout (not on stale responses).
    // RFC 6886 §3.2/§3.3: public-address responses are 12 bytes; port-mapping responses 16 bytes.
    fn transact(&self, request: &[u8], expected_opcode: u8) -> Result<[u8; 16]> {
        let min_len: usize = if expected_opcode == 128 { 12 } else { 16 };
        let mut delay_ms = 250u64;
        let mut buf = [0u8; 16];

        self.socket.send(request).context("send NAT-PMP request")?;

        for attempt in 0..MAX_ATTEMPTS {
            self.socket
                .set_read_timeout(Some(Duration::from_millis(delay_ms)))
                .context("set socket read timeout")?;

            loop {
                match self.socket.recv(&mut buf) {
                    Ok(n) if n >= 4 => {
                        if buf[0] != 0 {
                            bail!("unsupported NAT-PMP version {}", buf[0]);
                        }
                        if buf[1] != expected_opcode {
                            // Stale response from a previous request — keep waiting
                            // within this timeout window without retransmitting.
                            continue;
                        }
                        let result_code = u16::from_be_bytes([buf[2], buf[3]]);
                        if result_code != 0 {
                            bail!("NAT-PMP server error: {}", natpmp_strerror(result_code));
                        }
                        if n < min_len {
                            bail!("NAT-PMP response truncated: got {n} bytes, need {min_len}");
                        }
                        return Ok(buf);
                    }
                    Ok(_) => break, // too short — treat as a failed attempt
                    Err(e) if is_timeout(&e) => break,
                    // ConnectionRefused = Linux/macOS ICMP port-unreachable.
                    // ConnectionReset   = Windows WSAECONNRESET (10054) for UDP to a closed port.
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
                        ) =>
                    {
                        let gw = self.gateway;
                        bail!(
                            "{gw} is not a NAT-PMP server or the VPN is not connected \
                             — verify that --gateway is set to the correct address"
                        );
                    }
                    Err(e) => bail!("recv from gateway {}: {e}", self.gateway),
                }
            }

            if attempt + 1 < MAX_ATTEMPTS {
                delay_ms = delay_ms.saturating_mul(2);
                self.socket
                    .send(request)
                    .context("retransmit NAT-PMP request")?;
            }
        }

        let gw = self.gateway;
        bail!("no response from gateway {gw} — VPN not connected or {gw} is not the NAT-PMP server")
    }

    /// # Errors
    /// Returns an error if the NAT-PMP request fails or the gateway does not respond.
    pub fn get_public_address(&self) -> Result<Ipv4Addr> {
        let buf = self.transact(&[0, 0], 128)?;
        Ok(Ipv4Addr::new(buf[8], buf[9], buf[10], buf[11]))
    }

    /// # Errors
    /// Returns an error if the NAT-PMP request fails, the gateway does not respond,
    /// or the gateway refuses the mapping (returns port 0 or lifetime 0).
    pub fn map_port(
        &self,
        private_port: u16,
        protocol: Protocol,
        lifetime: u32,
    ) -> Result<PortMapping> {
        let (req_opcode, resp_opcode) = match protocol {
            Protocol::Udp => (1u8, 129u8),
            Protocol::Tcp => (2u8, 130u8),
        };

        let mut req = [0u8; 12];
        req[1] = req_opcode;
        req[4..6].copy_from_slice(&private_port.to_be_bytes());
        // req[6..8] = 0 — public port, let server assign
        req[8..12].copy_from_slice(&lifetime.to_be_bytes());

        let buf = self.transact(&req, resp_opcode)?;

        let mapping = PortMapping {
            private_port: u16::from_be_bytes([buf[8], buf[9]]),
            public_port: u16::from_be_bytes([buf[10], buf[11]]),
            lifetime: u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]),
        };

        if mapping.public_port == 0 || mapping.lifetime == 0 {
            bail!(
                "NAT-PMP: gateway refused mapping (public_port={}, lifetime={})",
                mapping.public_port,
                mapping.lifetime
            );
        }

        Ok(mapping)
    }
}

/// Validates a single candidate NAT-PMP response buffer.
///
/// Mirrors the per-datagram checks inside `transact()` but as a pure function
/// so that fuzz targets and property tests can drive it without socket I/O.
/// Opcode mismatches (stale responses) are treated as errors here rather than
/// "keep waiting", since this function handles only one candidate at a time.
#[cfg(feature = "fuzz")]
pub fn validate_response_bytes(data: &[u8], expected_opcode: u8) -> Result<[u8; 16]> {
    let n = data.len();
    let min_len: usize = if expected_opcode == 128 { 12 } else { 16 };

    if n < 4 {
        bail!("response too short: {n} bytes");
    }
    if data[0] != 0 {
        bail!("unsupported NAT-PMP version {}", data[0]);
    }
    if data[1] != expected_opcode {
        bail!("unexpected opcode {} (expected {expected_opcode})", data[1]);
    }
    let result_code = u16::from_be_bytes([data[2], data[3]]);
    if result_code != 0 {
        bail!("NAT-PMP server error: {}", natpmp_strerror(result_code));
    }
    if n < min_len {
        bail!("NAT-PMP response truncated: got {n} bytes, need {min_len}");
    }
    let mut buf = [0u8; 16];
    buf[..n.min(16)].copy_from_slice(&data[..n.min(16)]);
    Ok(buf)
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

const fn natpmp_strerror(code: u16) -> &'static str {
    match code {
        1 => "unsupported version",
        2 => "not authorized",
        3 => "network failure",
        4 => "out of resources",
        5 => "unsupported opcode",
        _ => "unknown error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_timeout_recognises_blocking_errors() {
        assert!(is_timeout(&io::Error::from(io::ErrorKind::WouldBlock)));
        assert!(is_timeout(&io::Error::from(io::ErrorKind::TimedOut)));
    }

    #[test]
    fn is_timeout_rejects_non_blocking_errors() {
        assert!(!is_timeout(&io::Error::from(
            io::ErrorKind::ConnectionRefused
        )));
        assert!(!is_timeout(&io::Error::from(io::ErrorKind::BrokenPipe)));
        assert!(!is_timeout(&io::Error::from(
            io::ErrorKind::ConnectionReset
        )));
        assert!(!is_timeout(&io::Error::from(io::ErrorKind::Other)));
    }

    #[test]
    fn natpmp_strerror_maps_all_rfc_codes() {
        assert_eq!(natpmp_strerror(1), "unsupported version");
        assert_eq!(natpmp_strerror(2), "not authorized");
        assert_eq!(natpmp_strerror(3), "network failure");
        assert_eq!(natpmp_strerror(4), "out of resources");
        assert_eq!(natpmp_strerror(5), "unsupported opcode");
    }

    #[test]
    fn natpmp_strerror_unknown_code_is_non_empty() {
        let msg = natpmp_strerror(6);
        assert!(!msg.is_empty(), "unknown code must still produce a message");
        assert_ne!(msg, "unsupported version");
        assert_ne!(msg, "not authorized");
    }

    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    #[test]
    fn bind_to_loopback_succeeds() {
        let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
        assert!(bind_to_interface(&socket, "lo0").is_ok());
    }

    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    #[test]
    fn bind_to_nonexistent_interface_errors() {
        let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
        let err = bind_to_interface(&socket, "nonexistent999").unwrap_err();
        assert!(format!("{err:#}").contains("not found"));
    }

    #[cfg(any(target_os = "macos", target_os = "freebsd"))]
    #[test]
    fn bind_to_interface_null_byte_in_name_errors() {
        let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
        let err = bind_to_interface(&socket, "lo\x000").unwrap_err();
        assert!(format!("{err:#}").contains("null byte"));
    }
}

// On Unix: bind to 0.0.0.0 then use SO_BINDTODEVICE / IP_BOUND_IF to pin the socket
// to the requested interface.
#[cfg(unix)]
fn make_socket(interface: Option<&str>) -> Result<UdpSocket> {
    let socket = UdpSocket::bind("0.0.0.0:0").context("bind UDP socket")?;
    if let Some(iface) = interface {
        bind_to_interface(&socket, iface)
            .with_context(|| format!("bind socket to interface {iface}"))?;
    }
    Ok(socket)
}

// On Windows: interface binding is done by binding directly to the interface's IP
// address rather than 0.0.0.0.
#[cfg(windows)]
fn make_socket(interface: Option<&str>) -> Result<UdpSocket> {
    let addr = match interface {
        Some(iface) => interface_ipv4(iface)
            .with_context(|| format!("find IPv4 address for interface {iface}"))?,
        None => Ipv4Addr::UNSPECIFIED,
    };
    UdpSocket::bind((addr, 0u16)).context("bind UDP socket")
}

#[cfg(not(any(unix, windows)))]
fn make_socket(_interface: Option<&str>) -> Result<UdpSocket> {
    bail!("this platform is not supported")
}

#[cfg(target_os = "linux")]
fn bind_to_interface(socket: &UdpSocket, iface: &str) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    let name = CString::new(iface).context("interface name contains null byte")?;
    let bytes = name.as_bytes_with_nul();
    let ret = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            bytes.as_ptr().cast::<libc::c_void>(),
            u32::try_from(bytes.len()).expect("interface name fits u32"),
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error()).context("SO_BINDTODEVICE");
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn bind_to_interface(socket: &UdpSocket, iface: &str) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    let name = CString::new(iface).context("interface name contains null byte")?;
    let idx = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if idx == 0 {
        bail!("interface '{iface}' not found — run 'ifconfig' to list available interfaces");
    }
    let ret = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            libc::IPPROTO_IP,
            libc::IP_BOUND_IF,
            (&raw const idx).cast::<libc::c_void>(),
            u32::try_from(std::mem::size_of::<libc::c_uint>()).expect("c_uint size fits u32"),
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error()).context("IP_BOUND_IF");
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))
))]
fn bind_to_interface(_socket: &UdpSocket, iface: &str) -> Result<()> {
    bail!("interface binding is not supported on this platform (requested: {iface})")
}

// Resolve a Windows network adapter name (e.g. "ProtonVPN") to its IPv4 address
// using GetAdaptersAddresses from the Windows IP Helper API.
#[cfg(windows)]
fn interface_ipv4(iface: &str) -> Result<Ipv4Addr> {
    use std::io;
    use windows_sys::Win32::Foundation::ERROR_BUFFER_OVERFLOW;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

    // Skip anycast, multicast, and DNS server entries — unicast only.
    const FLAGS: u32 = 0x0002 | 0x0004 | 0x0008; // GAA_FLAG_SKIP_ANYCAST | _MULTICAST | _DNS_SERVER

    let iface_wide: Vec<u16> = iface.encode_utf16().collect();
    let mut buf_len: u32 = 16_384;

    for _ in 0..4 {
        // Allocate as u64 to guarantee the 8-byte alignment required by
        // IP_ADAPTER_ADDRESSES_LH; buf_len is still tracked in bytes.
        let mut buf: Vec<u64> = vec![0u64; (buf_len as usize).div_ceil(8)];
        let ret = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_INET),
                FLAGS,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                &raw mut buf_len,
            )
        };

        if ret == ERROR_BUFFER_OVERFLOW {
            // buf_len updated by the API; retry with the larger allocation.
            continue;
        }
        if ret != 0 {
            return Err(io::Error::from_raw_os_error(ret.cast_signed()))
                .context("GetAdaptersAddresses");
        }

        let mut p = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while !p.is_null() {
            let adapter = unsafe { &*p };
            let name: Vec<u16> = unsafe {
                let ptr = adapter.FriendlyName;
                let mut len = 0usize;
                while *ptr.add(len) != 0 {
                    len += 1;
                }
                std::slice::from_raw_parts(ptr, len).to_vec()
            };

            if name == iface_wide {
                let mut ua = adapter.FirstUnicastAddress;
                while !ua.is_null() {
                    let ua_ref = unsafe { &*ua };
                    let sa_ptr = ua_ref.Address.lpSockaddr;
                    if !sa_ptr.is_null() {
                        // SOCKADDR pointer may be less aligned than SOCKADDR_IN requires;
                        // read_unaligned avoids undefined behaviour from the cast.
                        let sa: SOCKADDR_IN = unsafe { std::ptr::read_unaligned(sa_ptr.cast()) };
                        if sa.sin_family == AF_INET {
                            let addr = unsafe { sa.sin_addr.S_un.S_addr };
                            return Ok(Ipv4Addr::from(addr.to_ne_bytes()));
                        }
                    }
                    ua = ua_ref.Next;
                }
                bail!("interface '{iface}' has no IPv4 address");
            }

            p = adapter.Next;
        }

        bail!("interface '{iface}' not found — run 'ipconfig' to list available interfaces");
    }

    bail!("GetAdaptersAddresses: buffer size kept growing unexpectedly")
}
