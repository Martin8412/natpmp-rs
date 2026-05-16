use std::net::Ipv4Addr;

use anyhow::{Context, Result, bail};

/// # Errors
/// Returns an error if `/proc/net/route` cannot be read or contains no default route.
#[cfg(target_os = "linux")]
pub fn default_gateway() -> Result<Ipv4Addr> {
    let content = std::fs::read_to_string("/proc/net/route").context("read /proc/net/route")?;
    parse_proc_net_route(&content)
}

/// Parses the text content of `/proc/net/route` and returns the IPv4 address
/// of the default gateway.  Exposed for fuzz testing and property tests.
///
/// # Errors
/// Returns an error if the content contains no default route.
#[cfg(target_os = "linux")]
#[cfg_attr(not(feature = "fuzz"), allow(dead_code))]
pub fn parse_proc_net_route(content: &str) -> Result<Ipv4Addr> {
    for line in content.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let dest = u32::from_str_radix(cols[1], 16).unwrap_or(1);
        if dest != 0 {
            continue;
        }
        let flags = u32::from_str_radix(cols[3], 16).unwrap_or(0);
        if flags & 0x0003 != 0x0003 {
            // RTF_UP | RTF_GATEWAY both required
            continue;
        }
        let gw = u32::from_str_radix(cols[2], 16).unwrap_or(0);
        if gw == 0 {
            continue;
        }
        return Ok(Ipv4Addr::from(gw.to_le_bytes()));
    }

    bail!("no default route found")
}

/// # Errors
/// Returns an error if the routing socket cannot be opened, the route query fails,
/// or no default IPv4 route is found.
///
/// # Panics
/// Panics if the platform defines `RTM_VERSION`, `RTM_GET`, `AF_INET`, or
/// `sockaddr_in` with values that don't fit their expected field widths — which
/// cannot happen on any supported platform.
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
pub fn default_gateway() -> Result<Ipv4Addr> {
    use std::io;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

    // A routing message is a rt_msghdr followed by the sockaddrs flagged in
    // rtm_addrs, packed in RTA bit order. Sending RTA_DST = 0.0.0.0 queries
    // the default route.
    #[repr(C)]
    struct Msg {
        hdr: libc::rt_msghdr,
        dst: libc::sockaddr_in,
    }

    // AF_ROUTE + SOCK_RAW gives direct access to the kernel routing table.
    let sock: OwnedFd = unsafe {
        let fd = libc::socket(libc::AF_ROUTE, libc::SOCK_RAW, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error()).context("socket(AF_ROUTE)");
        }
        OwnedFd::from_raw_fd(fd)
    };

    let pid = unsafe { libc::getpid() };
    let seq: libc::c_int = 1;

    let mut msg: Msg = unsafe { std::mem::zeroed() };
    msg.hdr.rtm_msglen = u16::try_from(std::mem::size_of::<Msg>()).expect("Msg fits u16");
    msg.hdr.rtm_version = u8::try_from(libc::RTM_VERSION).expect("RTM_VERSION fits u8");
    msg.hdr.rtm_type = u8::try_from(libc::RTM_GET).expect("RTM_GET fits u8");
    msg.hdr.rtm_addrs = libc::RTA_DST;
    msg.hdr.rtm_seq = seq;
    msg.hdr.rtm_pid = pid;
    msg.dst.sin_len = u8::try_from(std::mem::size_of::<libc::sockaddr_in>()).expect("sockaddr_in fits u8");
    msg.dst.sin_family = u8::try_from(libc::AF_INET).expect("AF_INET fits u8");
    // sin_addr = INADDR_ANY (0.0.0.0) — requests the default route

    let ret = unsafe {
        libc::write(
            sock.as_raw_fd(),
            (&raw const msg).cast::<libc::c_void>(),
            std::mem::size_of::<Msg>(),
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error()).context("write RTM_GET");
    }

    // The routing socket may receive messages from other processes; loop until
    // we see a reply matching our own pid and sequence number.
    let mut buf = [0u8; 2048];
    loop {
        let n = unsafe {
            libc::read(
                sock.as_raw_fd(),
                buf.as_mut_ptr().cast::<libc::c_void>(),
                buf.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error()).context("read routing socket");
        }
        let n = n.cast_unsigned();
        if n < std::mem::size_of::<libc::rt_msghdr>() {
            continue;
        }

        // Safety: length checked above; rt_msghdr has no invalid bit patterns.
        let hdr: libc::rt_msghdr =
            unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };

        if hdr.rtm_pid != pid || hdr.rtm_seq != seq {
            continue;
        }
        if hdr.rtm_errno != 0 {
            bail!("no default route found");
        }

        let sa_start = std::mem::size_of::<libc::rt_msghdr>();
        return gateway_from_rtaddrs(hdr.rtm_addrs, &buf[sa_start..n]);
    }
}

// Walks the packed sockaddr sequence appended to a routing message and returns
// the IPv4 address in the RTA_GATEWAY slot.
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn gateway_from_rtaddrs(addrs: libc::c_int, data: &[u8]) -> Result<Ipv4Addr> {
    let mut offset = 0;

    for bit in 0..libc::RTAX_MAX {
        let rta_flag = 1i32 << bit;
        if addrs & rta_flag == 0 {
            continue;
        }
        if offset >= data.len() {
            break;
        }

        let sa_len = data[offset] as usize;

        if rta_flag == libc::RTA_GATEWAY {
            let sa_family = if offset + 1 < data.len() { data[offset + 1] } else { 0 };
            if sa_family != u8::try_from(libc::AF_INET).expect("AF_INET fits u8") {
                bail!("default gateway is not IPv4 (sa_family={sa_family})");
            }
            if sa_len < std::mem::size_of::<libc::sockaddr_in>()
                || offset + sa_len > data.len()
            {
                bail!("RTA_GATEWAY sockaddr is truncated");
            }
            // Safety: bounds checked above; sockaddr_in has no invalid bit patterns.
            let sin: libc::sockaddr_in =
                unsafe { std::ptr::read_unaligned(data.as_ptr().add(offset).cast()) };
            // s_addr is in network byte order; to_ne_bytes() yields the octets
            // as they sit in memory, which is what Ipv4Addr::from([u8; 4]) expects.
            return Ok(Ipv4Addr::from(sin.sin_addr.s_addr.to_ne_bytes()));
        }

        // Advance past this sockaddr, rounding up to the platform's long size.
        offset += if sa_len == 0 {
            std::mem::size_of::<libc::c_long>()
        } else {
            sa_roundup(sa_len)
        };
    }

    bail!("no default route found")
}

// BSD sockaddr alignment rule: round each sa_len up to sizeof(long).
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn sa_roundup(len: usize) -> usize {
    let align = std::mem::size_of::<libc::c_long>();
    (len + align - 1) & !(align - 1)
}

/// # Errors
/// Returns an error if `GetIpForwardTable2` fails or no default route is found.
#[cfg(windows)]
pub fn default_gateway() -> Result<Ipv4Addr> {
    use std::io;
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIpForwardTable2, MIB_IPFORWARD_ROW2, MIB_IPFORWARD_TABLE2,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    struct Guard(*mut MIB_IPFORWARD_TABLE2);
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { FreeMibTable(self.0.cast()) };
        }
    }

    let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
    let err = unsafe { GetIpForwardTable2(AF_INET, &raw mut table) };
    if err != 0 {
        return Err(io::Error::from_raw_os_error(err.cast_signed())).context("GetIpForwardTable2");
    }
    let _guard = Guard(table);

    let entries: &[MIB_IPFORWARD_ROW2] = unsafe {
        let t = &*table;
        std::slice::from_raw_parts(t.Table.as_ptr(), t.NumEntries as usize)
    };

    // Default route = prefix length 0 (matches 0.0.0.0/0).
    // Multiple defaults can exist; pick the one with the lowest metric.
    match entries
        .iter()
        .filter(|e| e.DestinationPrefix.PrefixLength == 0)
        .min_by_key(|e| e.Metric)
    {
        Some(e) => {
            let addr = unsafe { e.NextHop.Ipv4.sin_addr.S_un.S_addr };
            Ok(Ipv4Addr::from(addr.to_ne_bytes()))
        }
        None => bail!("no default route found; use --gateway"),
    }
}

/// # Errors
/// Always returns an error — auto-detection is not supported on this platform.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd", windows)))]
pub fn default_gateway() -> Result<Ipv4Addr> {
    bail!("auto-detection of the default gateway is not supported on this platform; use --gateway")
}

#[cfg(test)]
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    // Builds a 16-byte sockaddr_in buffer with `ip` in network byte order.
    fn make_sockaddr_in(ip: Ipv4Addr) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0] = 16; // sin_len
        b[1] = u8::try_from(libc::AF_INET).expect("AF_INET fits u8");
        b[4..8].copy_from_slice(&ip.octets());
        b
    }

    #[test]
    fn gateway_only_returns_correct_ip() {
        let data = make_sockaddr_in(Ipv4Addr::new(10, 2, 0, 1));
        assert_eq!(
            gateway_from_rtaddrs(libc::RTA_GATEWAY, &data).unwrap(),
            Ipv4Addr::new(10, 2, 0, 1)
        );
    }

    #[test]
    fn dst_before_gateway_advances_offset_correctly() {
        // RTA_DST (bit 0) precedes RTA_GATEWAY (bit 1); each sockaddr_in is 16 bytes.
        let mut data = [0u8; 32];
        data[..16].copy_from_slice(&make_sockaddr_in(Ipv4Addr::UNSPECIFIED));
        data[16..].copy_from_slice(&make_sockaddr_in(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(
            gateway_from_rtaddrs(libc::RTA_DST | libc::RTA_GATEWAY, &data).unwrap(),
            Ipv4Addr::new(192, 168, 1, 1)
        );
    }

    #[test]
    fn missing_gateway_bit_returns_err() {
        let data = make_sockaddr_in(Ipv4Addr::UNSPECIFIED);
        let err = gateway_from_rtaddrs(libc::RTA_DST, &data).unwrap_err();
        assert!(format!("{err:#}").contains("no default route"));
    }

    #[test]
    fn non_ipv4_gateway_returns_err() {
        let mut data = [0u8; 16];
        data[0] = 16;
        data[1] = u8::try_from(libc::AF_INET6).expect("AF_INET6 fits u8");
        let err = gateway_from_rtaddrs(libc::RTA_GATEWAY, &data).unwrap_err();
        assert!(format!("{err:#}").contains("not IPv4"));
    }

    #[test]
    fn truncated_gateway_sockaddr_returns_err() {
        let mut data = [0u8; 16];
        data[0] = 4; // sa_len < size_of::<sockaddr_in>() = 16
        data[1] = libc::AF_INET as u8;
        let err = gateway_from_rtaddrs(libc::RTA_GATEWAY, &data).unwrap_err();
        assert!(format!("{err:#}").contains("truncated"));
    }

    // data is exactly 1 byte: sa_family read is on the boundary (offset+1 == data.len()).
    // Distinguishes `offset + 1 < data.len()` from both `<=` and `offset * 1 < data.len()`.
    #[test]
    fn sa_family_read_at_exact_boundary_returns_not_ipv4() {
        let data = [0u8; 1]; // sa_len=0, no second byte
        let err = gateway_from_rtaddrs(libc::RTA_GATEWAY, &data).unwrap_err();
        assert!(format!("{err:#}").contains("not IPv4"));
    }

    // sa_len equals sockaddr_in size (first condition false), but the buffer is
    // shorter than offset+sa_len (second condition true).  Distinguishes `> data.len()`
    // from `< data.len()` in the truncation guard.
    #[test]
    fn gateway_data_shorter_than_sa_len_returns_truncated() {
        let mut data = [0u8; 4];
        data[0] = 16; // sa_len = size_of::<sockaddr_in>() — first || arm is false
        data[1] = libc::AF_INET as u8;
        // offset(0) + sa_len(16) = 16 > data.len()(4) → truncated
        let err = gateway_from_rtaddrs(libc::RTA_GATEWAY, &data).unwrap_err();
        assert!(format!("{err:#}").contains("truncated"));
    }
}
