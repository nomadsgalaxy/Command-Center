//! The host's LAN address, which its key screen shows as tags and the Frame reads back (docs/pairing.md 3).
//! The address is only where to connect; the key is still the secret and the pairing handshake still
//! gives the host's pinned key. So all that's checked is that it's a private IPv4 address.
use std::net::{Ipv4Addr, UdpSocket};

/// True for RFC 1918's 10/8, 172.16/12 and 192.168/16, and nothing else (not link-local, CGNAT or loopback).
pub fn private_v4(a: [u8; 4]) -> bool {
    Ipv4Addr::from(a).is_private()
}

/// Returns the IPv4 source address of the default route, which is the one a Frame on the LAN would
/// reach this host at. With several routes the kernel picks (its lowest metric wins). A UDP
/// connect sends nothing: it only asks the kernel which address it'd use. None when there's no
/// route, or when that address isn't private (a public one is not something to put on a screen).
pub fn route_source() -> Option<[u8; 4]> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:80").ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(a) if private_v4(a.octets()) => Some(a.octets()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_means_rfc1918() {
        for a in [[10, 0, 0, 1], [10, 255, 255, 255], [172, 16, 0, 1], [172, 31, 255, 254], [192, 168, 1, 20]] {
            assert!(private_v4(a), "{a:?}");
        }
        for a in [[8, 8, 8, 8], [172, 15, 0, 1], [172, 32, 0, 1], [192, 169, 0, 1], [127, 0, 0, 1], [169, 254, 1, 1], [100, 64, 0, 1], [0, 0, 0, 0], [255, 255, 255, 255], [224, 0, 0, 251]] {
            assert!(!private_v4(a), "{a:?}");
        }
    }

    #[test]
    fn the_route_source_is_private_or_nothing() {
        assert!(route_source().is_none_or(private_v4));
    }
}
