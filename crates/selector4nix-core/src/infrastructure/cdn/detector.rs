use std::fmt::{Display, Formatter, Result as FmtResult};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use reqwest::header::HeaderMap;

use crate::domain::common::url::Url;
use crate::domain::substituter::model::EndpointOptimizationKind;
use crate::infrastructure::dns::doh_resolver::{DohLookup, DohResolver};

const HEADER_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Cloudflare's published IPv4 edge ranges.
const CLOUDFLARE_IPV4_RANGES: &[Ipv4Range] = &[
    Ipv4Range::new([173, 245, 48, 0], 20),
    Ipv4Range::new([103, 21, 244, 0], 22),
    Ipv4Range::new([103, 22, 200, 0], 22),
    Ipv4Range::new([103, 31, 4, 0], 22),
    Ipv4Range::new([141, 101, 64, 0], 18),
    Ipv4Range::new([108, 162, 192, 0], 18),
    Ipv4Range::new([190, 93, 240, 0], 20),
    Ipv4Range::new([188, 114, 96, 0], 20),
    Ipv4Range::new([197, 234, 240, 0], 22),
    Ipv4Range::new([198, 41, 128, 0], 17),
    Ipv4Range::new([162, 158, 0, 0], 15),
    Ipv4Range::new([104, 16, 0, 0], 13),
    Ipv4Range::new([104, 24, 0, 0], 14),
    Ipv4Range::new([172, 64, 0, 0], 13),
    Ipv4Range::new([131, 0, 72, 0], 22),
];

/// Fastly's published IPv4 edge ranges.
const FASTLY_IPV4_RANGES: &[Ipv4Range] = &[
    Ipv4Range::new([23, 235, 32, 0], 20),
    Ipv4Range::new([43, 249, 72, 0], 22),
    Ipv4Range::new([103, 244, 50, 0], 24),
    Ipv4Range::new([103, 245, 222, 0], 23),
    Ipv4Range::new([103, 245, 224, 0], 24),
    Ipv4Range::new([104, 156, 80, 0], 20),
    Ipv4Range::new([140, 248, 64, 0], 18),
    Ipv4Range::new([140, 248, 128, 0], 17),
    Ipv4Range::new([146, 75, 0, 0], 17),
    Ipv4Range::new([151, 101, 0, 0], 16),
    Ipv4Range::new([157, 52, 64, 0], 18),
    Ipv4Range::new([167, 82, 0, 0], 17),
    Ipv4Range::new([167, 82, 128, 0], 20),
    Ipv4Range::new([167, 82, 160, 0], 20),
    Ipv4Range::new([167, 82, 224, 0], 20),
    Ipv4Range::new([172, 111, 64, 0], 18),
    Ipv4Range::new([185, 31, 16, 0], 22),
    Ipv4Range::new([199, 27, 72, 0], 21),
    Ipv4Range::new([199, 232, 0, 0], 16),
];

#[derive(Debug, Clone, Copy)]
struct Ipv4Range {
    network: u32,
    prefix: u8,
}

impl Ipv4Range {
    const fn new(octets: [u8; 4], prefix: u8) -> Self {
        Self {
            network: u32::from_be_bytes(octets),
            prefix,
        }
    }

    fn contains(self, address: Ipv4Addr) -> bool {
        let mask = u32::MAX << (32 - self.prefix);
        u32::from(address) & mask == self.network & mask
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdnDetectionEvidence {
    Address(Ipv4Addr),
    Cname(String),
    HttpHeader(&'static str),
}

impl Display for CdnDetectionEvidence {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Address(address) => write!(formatter, "IP {address}"),
            Self::Cname(cname) => write!(formatter, "CNAME {cname}"),
            Self::HttpHeader(header) => write!(formatter, "HTTP header {header}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdnDetection {
    pub kind: EndpointOptimizationKind,
    pub evidence: CdnDetectionEvidence,
}

/// Detects whether a substituter is served by Cloudflare or Fastly. DNS is
/// checked first (published edge ranges, then CNAME suffixes); an HTTP request
/// for `nix-cache-info` supplies a response-header fallback for setups whose
/// DNS chain does not expose the CDN directly.
pub struct CdnDetector {
    doh: Arc<DohResolver>,
    client: Client,
}

impl CdnDetector {
    pub fn new(doh: Arc<DohResolver>, client: Client) -> Self {
        Self { doh, client }
    }

    pub async fn detect(&self, base_url: &Url) -> Option<CdnDetection> {
        let host = base_url.host();
        let lookup = self.doh.query_a_with_aliases(host).await;
        if let Some(detection) = detect_from_dns(&lookup) {
            return Some(detection);
        }

        let probe_url = match base_url.as_dir().join("nix-cache-info") {
            Ok(url) => url,
            Err(error) => {
                tracing::debug!(%host, %error, "could not build CDN header probe URL");
                return None;
            }
        };
        match self
            .client
            .get(probe_url.value())
            .timeout(HEADER_PROBE_TIMEOUT)
            .send()
            .await
        {
            Ok(response) => detect_from_headers(response.headers()),
            Err(error) => {
                tracing::debug!(%host, %error, "CDN response-header probe failed");
                None
            }
        }
    }
}

fn detect_from_dns(lookup: &DohLookup) -> Option<CdnDetection> {
    let cloudflare_address = lookup
        .addresses
        .iter()
        .copied()
        .find(|address| in_ranges(*address, CLOUDFLARE_IPV4_RANGES));
    let fastly_address = lookup
        .addresses
        .iter()
        .copied()
        .find(|address| in_ranges(*address, FASTLY_IPV4_RANGES));

    match (cloudflare_address, fastly_address) {
        (Some(address), None) => {
            return Some(CdnDetection {
                kind: EndpointOptimizationKind::Cloudflare,
                evidence: CdnDetectionEvidence::Address(address),
            });
        }
        (None, Some(address)) => {
            return Some(CdnDetection {
                kind: EndpointOptimizationKind::Fastly,
                evidence: CdnDetectionEvidence::Address(address),
            });
        }
        // Conflicting A records are not enough evidence to safely choose a
        // platform. Continue with the CNAME chain and then response headers.
        (Some(_), Some(_)) | (None, None) => {}
    }

    let cloudflare_cname = lookup
        .aliases
        .iter()
        .find(|alias| has_domain_suffix(alias, "cloudflare.net"));
    let fastly_cname = lookup.aliases.iter().find(|alias| {
        has_domain_suffix(alias, "fastly.net") || has_domain_suffix(alias, "fastlylb.net")
    });

    match (cloudflare_cname, fastly_cname) {
        (Some(cname), None) => Some(CdnDetection {
            kind: EndpointOptimizationKind::Cloudflare,
            evidence: CdnDetectionEvidence::Cname((*cname).clone()),
        }),
        (None, Some(cname)) => Some(CdnDetection {
            kind: EndpointOptimizationKind::Fastly,
            evidence: CdnDetectionEvidence::Cname((*cname).clone()),
        }),
        _ => None,
    }
}

fn detect_from_headers(headers: &HeaderMap) -> Option<CdnDetection> {
    if headers.contains_key("cf-ray") {
        return Some(header_detection(
            EndpointOptimizationKind::Cloudflare,
            "cf-ray",
        ));
    }
    if headers.contains_key("cf-cache-status") {
        return Some(header_detection(
            EndpointOptimizationKind::Cloudflare,
            "cf-cache-status",
        ));
    }
    if header_contains(headers, "server", "cloudflare") {
        return Some(header_detection(
            EndpointOptimizationKind::Cloudflare,
            "server",
        ));
    }

    if header_contains(headers, "server", "fastly") {
        return Some(header_detection(EndpointOptimizationKind::Fastly, "server"));
    }
    if headers.contains_key("fastly-debug-digest") {
        return Some(header_detection(
            EndpointOptimizationKind::Fastly,
            "fastly-debug-digest",
        ));
    }
    if headers.contains_key("x-served-by")
        && (headers.contains_key("x-cache")
            || headers.contains_key("x-cache-hits")
            || headers.contains_key("x-timer"))
    {
        return Some(header_detection(
            EndpointOptimizationKind::Fastly,
            "x-served-by + cache headers",
        ));
    }
    None
}

fn header_detection(kind: EndpointOptimizationKind, header: &'static str) -> CdnDetection {
    CdnDetection {
        kind,
        evidence: CdnDetectionEvidence::HttpHeader(header),
    }
}

fn header_contains(headers: &HeaderMap, name: &str, expected: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains(expected))
}

fn has_domain_suffix(value: &str, suffix: &str) -> bool {
    let value = value.trim_end_matches('.').to_ascii_lowercase();
    value == suffix || value.ends_with(&format!(".{suffix}"))
}

fn in_ranges(address: Ipv4Addr, ranges: &[Ipv4Range]) -> bool {
    ranges.iter().any(|range| range.contains(address))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(addresses: &[&str], aliases: &[&str]) -> DohLookup {
        DohLookup {
            addresses: addresses
                .iter()
                .map(|address| address.parse().unwrap())
                .collect(),
            aliases: aliases.iter().map(|alias| (*alias).to_string()).collect(),
        }
    }

    #[test]
    fn detects_fastly_from_published_address_range() {
        let detection = detect_from_dns(&lookup(&["151.101.65.91"], &[])).unwrap();
        assert_eq!(detection.kind, EndpointOptimizationKind::Fastly);
        assert_eq!(
            detection.evidence,
            CdnDetectionEvidence::Address(Ipv4Addr::new(151, 101, 65, 91))
        );
    }

    #[test]
    fn detects_cloudflare_from_published_address_range() {
        let detection = detect_from_dns(&lookup(&["104.16.12.34"], &[])).unwrap();
        assert_eq!(detection.kind, EndpointOptimizationKind::Cloudflare);
    }

    #[test]
    fn detects_platform_from_cname_when_address_is_not_published() {
        let fastly = detect_from_dns(&lookup(
            &["192.0.2.10"],
            &["dualstack.example.sni.global.fastly.net."],
        ))
        .unwrap();
        assert_eq!(fastly.kind, EndpointOptimizationKind::Fastly);

        let cloudflare =
            detect_from_dns(&lookup(&["192.0.2.11"], &["customer.cdn.cloudflare.net"])).unwrap();
        assert_eq!(cloudflare.kind, EndpointOptimizationKind::Cloudflare);
    }

    #[test]
    fn conflicting_platform_addresses_are_not_classified() {
        assert!(detect_from_dns(&lookup(&["104.16.12.34", "151.101.65.91"], &[])).is_none());
    }

    #[test]
    fn detects_cloudflare_response_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-ray", "abc-SJC".parse().unwrap());
        let detection = detect_from_headers(&headers).unwrap();
        assert_eq!(detection.kind, EndpointOptimizationKind::Cloudflare);
    }

    #[test]
    fn detects_fastly_response_header_combination() {
        let mut headers = HeaderMap::new();
        headers.insert("x-served-by", "cache-sjc10045-SJC".parse().unwrap());
        headers.insert("x-cache", "HIT".parse().unwrap());
        let detection = detect_from_headers(&headers).unwrap();
        assert_eq!(detection.kind, EndpointOptimizationKind::Fastly);
    }

    #[test]
    fn generic_headers_and_addresses_are_not_classified() {
        let mut headers = HeaderMap::new();
        headers.insert("server", "nginx".parse().unwrap());
        assert!(detect_from_headers(&headers).is_none());
        assert!(detect_from_dns(&lookup(&["192.0.2.1"], &[])).is_none());
    }

    #[test]
    fn range_boundaries_are_inclusive() {
        assert!(in_ranges(
            Ipv4Addr::new(173, 245, 48, 0),
            CLOUDFLARE_IPV4_RANGES
        ));
        assert!(in_ranges(
            Ipv4Addr::new(173, 245, 63, 255),
            CLOUDFLARE_IPV4_RANGES
        ));
        assert!(!in_ranges(
            Ipv4Addr::new(173, 245, 64, 0),
            CLOUDFLARE_IPV4_RANGES
        ));
    }
}
