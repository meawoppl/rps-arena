use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use axum::{
    async_trait,
    extract::{ConnectInfo, FromRequestParts, Request},
    http::{request::Parts, HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};

/// Canonical client IP for abuse controls.
///
/// `capture_client_ip` inserts this into request extensions. Handlers can
/// consume it directly as an extractor: `ClientIp(ip): ClientIp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientIp(pub IpAddr);

#[async_trait]
impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<ClientIp>()
            .copied()
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

/// Middleware that records the peer IP, or a trusted forwarded IP when the
/// immediate peer is a configured/trusted proxy.
pub async fn capture_client_ip(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request,
    next: Next,
) -> Response {
    let client_ip = resolve_client_ip(req.headers(), peer.ip());
    req.extensions_mut().insert(ClientIp(client_ip));
    next.run(req).await
}

/// Resolve the canonical client IP from a socket peer and trusted proxy
/// headers. This is shared by the request extension middleware and edge rate
/// limiting so both controls bucket clients the same way.
pub fn resolve_client_ip(headers: &HeaderMap, peer_ip: IpAddr) -> IpAddr {
    let trusted_proxies = TrustedProxies::from_env();
    forwarded_client_ip(headers, peer_ip, &trusted_proxies).unwrap_or(peer_ip)
}

fn forwarded_client_ip(
    headers: &HeaderMap,
    peer_ip: IpAddr,
    trusted_proxies: &TrustedProxies,
) -> Option<IpAddr> {
    if !trusted_proxies.contains(peer_ip) {
        return None;
    }

    header_ip(headers, "cf-connecting-ip")
        .or_else(|| header_ip(headers, "fly-client-ip"))
        .or_else(|| header_ip(headers, "x-real-ip"))
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(',').next())
                .and_then(parse_forwarded_ip)
        })
}

fn header_ip(headers: &HeaderMap, name: &'static str) -> Option<IpAddr> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_forwarded_ip)
}

fn parse_forwarded_ip(raw: &str) -> Option<IpAddr> {
    let value = raw.trim();
    value.parse().ok()
}

#[derive(Debug, Clone)]
struct TrustedProxies {
    ranges: Vec<IpRange>,
}

impl TrustedProxies {
    fn from_env() -> Self {
        let ranges = std::env::var("TRUSTED_PROXY_CIDRS")
            .ok()
            .into_iter()
            .flat_map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter_map(|raw| IpRange::parse(&raw))
            .collect();
        Self { ranges }
    }

    fn contains(&self, ip: IpAddr) -> bool {
        ip.is_loopback() || is_private_ip(ip) || self.ranges.iter().any(|range| range.contains(ip))
    }
}

#[derive(Debug, Clone, Copy)]
enum IpRange {
    V4 { network: u32, prefix: u8 },
    V6 { network: u128, prefix: u8 },
}

impl IpRange {
    fn parse(raw: &str) -> Option<Self> {
        if raw.is_empty() {
            return None;
        }
        let (addr, prefix) = match raw.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix.parse::<u8>().ok()?)),
            None => (raw, None),
        };
        match addr.parse::<IpAddr>().ok()? {
            IpAddr::V4(addr) => {
                let prefix = prefix.unwrap_or(32);
                if prefix > 32 {
                    return None;
                }
                Some(Self::V4 {
                    network: u32::from(addr) & prefix_mask_v4(prefix),
                    prefix,
                })
            }
            IpAddr::V6(addr) => {
                let prefix = prefix.unwrap_or(128);
                if prefix > 128 {
                    return None;
                }
                Some(Self::V6 {
                    network: u128::from(addr) & prefix_mask_v6(prefix),
                    prefix,
                })
            }
        }
    }

    fn contains(self, ip: IpAddr) -> bool {
        match (self, ip) {
            (Self::V4 { network, prefix }, IpAddr::V4(ip)) => {
                (u32::from(ip) & prefix_mask_v4(prefix)) == network
            }
            (Self::V6 { network, prefix }, IpAddr::V6(ip)) => {
                (u128::from(ip) & prefix_mask_v6(prefix)) == network
            }
            _ => false,
        }
    }
}

fn prefix_mask_v4(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

fn prefix_mask_v6(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    }
}

fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private(),
        IpAddr::V6(ip) => is_unique_local_v6(ip),
    }
}

fn is_unique_local_v6(ip: Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::net::Ipv4Addr;

    #[test]
    fn untrusted_peer_cannot_spoof_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.99"));
        let proxies = TrustedProxies { ranges: Vec::new() };
        assert_eq!(
            forwarded_client_ip(&headers, IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), &proxies),
            None
        );
    }

    #[test]
    fn trusted_peer_uses_first_x_forwarded_for_ip() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.99, 10.0.0.1"),
        );
        let proxies = TrustedProxies { ranges: Vec::new() };
        assert_eq!(
            forwarded_client_ip(&headers, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), &proxies),
            Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 99)))
        );
    }

    #[test]
    fn explicit_cidr_matches_proxy_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.10"));
        let proxies = TrustedProxies {
            ranges: vec![IpRange::parse("192.0.2.0/24").unwrap()],
        };
        assert_eq!(
            forwarded_client_ip(&headers, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 44)), &proxies),
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)))
        );
    }

    #[test]
    fn cidr_parser_rejects_invalid_prefixes() {
        assert!(IpRange::parse("192.0.2.1/33").is_none());
        assert!(IpRange::parse("2001:db8::1/129").is_none());
        assert!(IpRange::parse("not-an-ip").is_none());
    }
}
