//! Proxy configuration from the conventional environment variables
//! (`HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`, `NO_PROXY`).

use bb_url::{Host, Url};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProxyAddr {
    pub host: String,
    pub port: u16,
    /// Pre-encoded `Basic xxx` credentials, if the proxy URL had userinfo.
    pub authorization: Option<String>,
}

#[derive(Debug, Clone)]
enum NoProxyRule {
    All,
    /// Domain suffix (leading dot stripped).
    Suffix(String),
    /// IPv4 network/prefix.
    Cidr4(u32, u32),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProxyConfig {
    https: Option<ProxyAddr>,
    http: Option<ProxyAddr>,
    no_proxy: Vec<NoProxyRule>,
}

fn env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| std::env::var(n).ok()).filter(|v| !v.trim().is_empty())
}

pub(crate) fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn parse_proxy(value: &str) -> Option<ProxyAddr> {
    let v = if value.contains("://") { value.to_string() } else { format!("http://{value}") };
    let u = Url::parse(&v).ok()?;
    if u.scheme() != "http" {
        log::warn!("unsupported proxy scheme {}", u.scheme());
        return None;
    }
    let host = u.host()?.connect_name();
    let authorization = if u.username().is_empty() {
        None
    } else {
        let dec =
            |s: &str| String::from_utf8_lossy(&bb_url::percent::percent_decode(s.as_bytes())).into_owned();
        let cred = format!("{}:{}", dec(u.username()), dec(u.password()));
        Some(format!("Basic {}", base64_encode(cred.as_bytes())))
    };
    Some(ProxyAddr { host, port: u.port_or_default().unwrap_or(80), authorization })
}

fn parse_no_proxy(value: &str) -> Vec<NoProxyRule> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            if s == "*" {
                return Some(NoProxyRule::All);
            }
            let s = s.trim_start_matches("*.").trim_start_matches('.');
            if let Some((net, bits)) = s.split_once('/') {
                let ip: std::net::Ipv4Addr = net.parse().ok()?;
                let bits: u32 = bits.parse().ok()?;
                let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits.min(32)) };
                return Some(NoProxyRule::Cidr4(u32::from(ip) & mask, mask));
            }
            Some(NoProxyRule::Suffix(s.to_ascii_lowercase()))
        })
        .collect()
}

impl ProxyConfig {
    pub fn from_env() -> Self {
        let all = env(&["ALL_PROXY", "all_proxy"]);
        let https = env(&["HTTPS_PROXY", "https_proxy"]).or_else(|| all.clone());
        let http = env(&["HTTP_PROXY", "http_proxy"]).or(all);
        ProxyConfig {
            https: https.as_deref().and_then(parse_proxy),
            http: http.as_deref().and_then(parse_proxy),
            no_proxy: env(&["NO_PROXY", "no_proxy"]).map(|v| parse_no_proxy(&v)).unwrap_or_default(),
        }
    }

    fn bypass(&self, host: &Host) -> bool {
        if host.is_loopback() {
            return true;
        }
        let name = host.connect_name();
        self.no_proxy.iter().any(|r| match r {
            NoProxyRule::All => true,
            NoProxyRule::Suffix(s) => name == *s || name.ends_with(&format!(".{s}")),
            NoProxyRule::Cidr4(net, mask) => match host {
                Host::Ipv4(a) => a & mask == *net,
                _ => false,
            },
        })
    }

    /// The proxy to use for `url`, if any.
    pub fn proxy_for(&self, url: &Url) -> Option<&ProxyAddr> {
        let host = url.host()?;
        if self.bypass(host) {
            return None;
        }
        match url.scheme() {
            "https" => self.https.as_ref(),
            "http" => self.http.as_ref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rules() {
        let cfg = ProxyConfig {
            https: parse_proxy("http://user:p%40ss@proxy.local:3128"),
            http: None,
            no_proxy: parse_no_proxy("localhost, .internal.example ,10.0.0.0/8"),
        };
        let p = cfg.proxy_for(&Url::parse("https://example.com/").unwrap()).unwrap();
        assert_eq!((p.host.as_str(), p.port), ("proxy.local", 3128));
        assert_eq!(p.authorization.as_deref(), Some("Basic dXNlcjpwQHNz"));
        assert!(cfg.proxy_for(&Url::parse("https://a.internal.example/").unwrap()).is_none());
        assert!(cfg.proxy_for(&Url::parse("https://10.1.2.3/").unwrap()).is_none());
        assert!(cfg.proxy_for(&Url::parse("https://127.0.0.1/").unwrap()).is_none());
        assert!(cfg.proxy_for(&Url::parse("http://example.com/").unwrap()).is_none());
    }
    #[test]
    fn b64() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b"hi"), "aGk=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }
}
