//! Who's asking, for rate limits and for the admin page's proxy check.
//!
//! The one header named by `CLIENT_IP_HEADER` is trusted, because the proxy in front
//! overwrites it. Anything a client could set for itself is never read.

use std::net::{IpAddr, SocketAddr};

use axum::{
    extract::ConnectInfo,
    http::{Extensions, HeaderMap},
};

use crate::server::config::Config;

/// The header's address if it holds exactly one, and otherwise the socket peer. A
/// surprising header means the proxy isn't doing what the settings say, and then the
/// peer is the only thing here that can't be forged.
pub fn resolve(config: &Config, headers: &HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
    let from_header = config.client_ip_header.as_ref().and_then(|name| {
        let mut values = headers.get_all(name).iter();
        let value = values.next()?;
        if values.next().is_some() {
            return None;
        }
        value.to_str().ok()?.trim().parse().ok()
    });
    from_header.or(peer)
}

pub fn from_request(
    config: &Config,
    headers: &HeaderMap,
    extensions: &Extensions,
) -> Option<IpAddr> {
    let peer = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    resolve(config, headers, peer)
}

/// What a limit counts against. One IPv6 customer usually has a whole /64 to hand
/// out addresses from, so that's what counts as one client.
pub fn limit_key(ip: Option<IpAddr>) -> String {
    match ip.map(|ip| ip.to_canonical()) {
        Some(IpAddr::V6(v6)) => {
            let [a, b, c, d, ..] = v6.segments();
            format!("{a:x}:{b:x}:{c:x}:{d:x}::/64")
        }
        Some(IpAddr::V4(v4)) => v4.to_string(),
        None => "unknown".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderName, HeaderValue};

    use super::*;

    fn config(header: Option<&'static str>) -> Config {
        Config::from_lookup(|name| match name {
            "PUBLIC_URL" => Some("https://books.example.com".to_owned()),
            "CLIENT_IP_HEADER" => header.map(str::to_owned),
            _ => None,
        })
        .unwrap()
    }

    fn headers(values: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in values {
            map.append(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            );
        }
        map
    }

    const PEER: Option<IpAddr> = Some(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1)));

    #[test]
    fn with_no_header_set_up_only_the_peer_counts() {
        let sent = headers(&[("x-real-ip", "1.2.3.4"), ("x-forwarded-for", "1.2.3.4")]);
        assert_eq!(resolve(&config(None), &sent, PEER), PEER);
    }

    #[test]
    fn the_configured_header_is_used() {
        let sent = headers(&[("x-real-ip", " 1.2.3.4 ")]);
        assert_eq!(
            resolve(&config(Some("X-Real-IP")), &sent, PEER),
            "1.2.3.4".parse().ok()
        );
    }

    #[test]
    fn other_forwarding_headers_are_never_read() {
        let sent = headers(&[("x-forwarded-for", "1.2.3.4"), ("forwarded", "for=1.2.3.4")]);
        assert_eq!(resolve(&config(Some("X-Real-IP")), &sent, PEER), PEER);
    }

    #[test]
    fn anything_but_exactly_one_address_falls_back_to_the_peer() {
        for sent in [
            headers(&[("x-real-ip", "1.2.3.4"), ("x-real-ip", "5.6.7.8")]),
            headers(&[("x-real-ip", "1.2.3.4, 5.6.7.8")]),
            headers(&[("x-real-ip", "not an address")]),
            headers(&[("x-real-ip", "")]),
            headers(&[]),
        ] {
            assert_eq!(resolve(&config(Some("X-Real-IP")), &sent, PEER), PEER);
        }
    }

    #[test]
    fn a_whole_ipv6_subnet_counts_as_one() {
        let one = limit_key("2001:db8:1:2:aaaa::1".parse().ok());
        let two = limit_key("2001:db8:1:2:bbbb::2".parse().ok());
        let other = limit_key("2001:db8:1:3::1".parse().ok());
        assert_eq!(one, two);
        assert_ne!(one, other);
    }

    #[test]
    fn a_mapped_ipv4_address_is_the_same_client() {
        assert_eq!(
            limit_key("::ffff:1.2.3.4".parse().ok()),
            limit_key("1.2.3.4".parse().ok())
        );
    }
}
