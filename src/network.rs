use anyhow::{anyhow, Context, Result};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

#[cfg(unix)]
use std::{cmp::Reverse, ffi::CStr, ptr};

const LISTEN_PORT: u16 = 41324;

pub fn listen_port() -> u16 {
    LISTEN_PORT
}

pub fn preferred_bind_addr() -> Result<SocketAddr> {
    preferred_bind_addr_for_port(LISTEN_PORT)
}

pub fn preferred_bind_addr_for_port(port: u16) -> Result<SocketAddr> {
    Ok(SocketAddr::from((preferred_bind_ip()?, port)))
}

pub fn preferred_bind_ip() -> Result<Ipv4Addr> {
    #[cfg(unix)]
    if let Some(ip) = find_preferred_private_ipv4()? {
        return Ok(ip);
    }

    if let Some(ip) = find_routed_private_ipv4()? {
        return Ok(ip);
    }

    Err(anyhow!("No private LAN IPv4 address is available"))
}

fn find_routed_private_ipv4() -> Result<Option<Ipv4Addr>> {
    let socket = UdpSocket::bind("0.0.0.0:0").context("failed to create route lookup socket")?;
    socket
        .connect("8.8.8.8:80")
        .context("failed to determine default IPv4 route")?;

    match socket
        .local_addr()
        .context("failed to read routed local address")?
        .ip()
    {
        IpAddr::V4(ip) if is_usable_private_ipv4(ip) => Ok(Some(ip)),
        _ => Ok(None),
    }
}

#[cfg(unix)]
fn find_preferred_private_ipv4() -> Result<Option<Ipv4Addr>> {
    let mut interfaces = ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut interfaces) } != 0 {
        return Err(std::io::Error::last_os_error()).context("getifaddrs failed");
    }

    let result = unsafe { collect_preferred_private_ipv4(interfaces) };
    unsafe { libc::freeifaddrs(interfaces) };
    result
}

#[cfg(unix)]
unsafe fn collect_preferred_private_ipv4(
    interfaces: *mut libc::ifaddrs,
) -> Result<Option<Ipv4Addr>> {
    let mut candidates = Vec::new();
    let mut current = interfaces;

    while !current.is_null() {
        let interface = &*current;

        if interface.ifa_addr.is_null() || interface.ifa_name.is_null() {
            current = interface.ifa_next;
            continue;
        }

        let flags = interface.ifa_flags as i32;
        if flags & libc::IFF_UP == 0
            || flags & libc::IFF_LOOPBACK != 0
            || flags & libc::IFF_POINTOPOINT != 0
        {
            current = interface.ifa_next;
            continue;
        }

        if (*interface.ifa_addr).sa_family as i32 != libc::AF_INET {
            current = interface.ifa_next;
            continue;
        }

        let name = CStr::from_ptr(interface.ifa_name)
            .to_string_lossy()
            .into_owned();
        if is_excluded_interface(&name) {
            current = interface.ifa_next;
            continue;
        }

        let sockaddr = *(interface.ifa_addr as *const libc::sockaddr_in);
        let ip = Ipv4Addr::from(u32::from_be(sockaddr.sin_addr.s_addr));
        if !is_usable_private_ipv4(ip) {
            current = interface.ifa_next;
            continue;
        }

        candidates.push((candidate_priority(&name), ip));
        current = interface.ifa_next;
    }

    candidates.sort_by_key(|(priority, ip)| (Reverse(*priority), Reverse(u32::from(*ip))));
    Ok(candidates.first().map(|(_, ip)| *ip))
}

#[cfg(unix)]
fn is_excluded_interface(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    [
        "lo", "utun", "tun", "tap", "ppp", "ipsec", "bridge", "awdl", "llw", "docker", "veth",
        "vmnet",
    ]
    .iter()
    .any(|prefix| lowered.starts_with(prefix))
}

#[cfg(unix)]
fn candidate_priority(name: &str) -> i32 {
    let lowered = name.to_ascii_lowercase();
    if lowered.starts_with("en") || lowered.starts_with("eth") {
        4
    } else if lowered.starts_with("wlan") || lowered.starts_with("wl") || lowered.starts_with("wi")
    {
        3
    } else if lowered.starts_with("bridge") || lowered.starts_with("br") {
        1
    } else {
        2
    }
}

fn is_usable_private_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_broadcast()
        && !ip.is_documentation()
        && !ip.is_unspecified()
}

#[cfg(test)]
mod tests {
    use super::{find_routed_private_ipv4, is_usable_private_ipv4};
    use std::net::Ipv4Addr;

    #[test]
    fn accepts_private_lan_addresses() {
        assert!(is_usable_private_ipv4(Ipv4Addr::new(192, 168, 1, 44)));
        assert!(is_usable_private_ipv4(Ipv4Addr::new(10, 0, 0, 5)));
    }

    #[test]
    fn rejects_non_lan_addresses() {
        assert!(!is_usable_private_ipv4(Ipv4Addr::new(84, 24, 1, 9)));
        assert!(!is_usable_private_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_usable_private_ipv4(Ipv4Addr::new(169, 254, 1, 2)));
    }

    #[cfg(unix)]
    #[test]
    fn prioritizes_primary_lan_interfaces() {
        use super::candidate_priority;

        assert!(candidate_priority("en0") > candidate_priority("bridge0"));
        assert!(candidate_priority("eth0") > candidate_priority("br0"));
    }

    #[test]
    fn routed_private_ipv4_lookup_does_not_panic() {
        let _ = find_routed_private_ipv4();
    }
}
