//! Minimal DNS-over-HTTPS resolver used for endpoint candidate discovery.
//!
//! Bypasses the system DNS resolver (which may return fake-ip addresses) by
//! querying the JSON DoH APIs of Cloudflare and Google over HTTPS, with
//! bootstrap IPs pinned via `ClientBuilder::resolve_to_addrs`.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;

const CLOUDFLARE_DOH_URL: &str = "https://cloudflare-dns.com/dns-query";
const GOOGLE_DOH_URL: &str = "https://dns.google/resolve";
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct DohResponse {
    #[serde(rename = "Answer", default)]
    answer: Vec<DohAnswer>,
}

#[derive(Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    record_type: u32,
    data: Option<String>,
}

/// A-record addresses and canonical-name aliases returned by a DoH lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DohLookup {
    pub addresses: Vec<Ipv4Addr>,
    pub aliases: Vec<String>,
}

/// Extract A records and CNAME aliases from a JSON DoH response body, skipping
/// entries with missing or unparseable data values.
fn parse_lookup(body: &str) -> DohLookup {
    let Ok(response) = serde_json::from_str::<DohResponse>(body) else {
        return DohLookup::default();
    };
    let mut lookup = DohLookup::default();
    for answer in response.answer {
        let Some(data) = answer.data else {
            continue;
        };
        match answer.record_type {
            1 => {
                if let Ok(address) = data.parse() {
                    lookup.addresses.push(address);
                }
            }
            5 => {
                let alias = data.trim_end_matches('.').to_ascii_lowercase();
                if !alias.is_empty() {
                    lookup.aliases.push(alias);
                }
            }
            _ => {}
        }
    }
    lookup
}

/// DNS-over-HTTPS resolver backed by Cloudflare and Google, with bootstrap
/// IPs pinned so no system DNS lookup is ever performed.
pub struct DohResolver {
    client: Client,
}

impl DohResolver {
    /// Build a resolver with a dedicated HTTP client that ignores proxies and
    /// resolves the DoH hostnames to pinned bootstrap addresses.
    pub fn new() -> Self {
        let client = Client::builder()
            .no_proxy()
            .resolve_to_addrs(
                "cloudflare-dns.com",
                &[
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1)), 443),
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(1, 0, 0, 1)), 443),
                ],
            )
            .resolve_to_addrs(
                "dns.google",
                &[
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 443),
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4)), 443),
                ],
            )
            .timeout(QUERY_TIMEOUT)
            .build()
            .expect("DoH HTTP client configuration is valid");
        Self { client }
    }

    /// Query both DoH resolvers for the A records of `name` and return the
    /// deduplicated union of the results. A single resolver failing only
    /// produces a warning; the other resolver's results are still returned.
    pub async fn query_a(&self, name: &str) -> Vec<Ipv4Addr> {
        self.query_a_with_aliases(name).await.addresses
    }

    /// Query both DoH resolvers for A records and the CNAME chain of `name`.
    /// The aliases are used by CDN platform detection while the addresses are
    /// also suitable for endpoint candidate discovery.
    pub async fn query_a_with_aliases(&self, name: &str) -> DohLookup {
        let (cloudflare, google) = tokio::join!(
            self.query_one(CLOUDFLARE_DOH_URL, name),
            self.query_one(GOOGLE_DOH_URL, name),
        );
        let mut addresses = BTreeSet::new();
        let mut aliases = BTreeSet::new();
        for (resolver, result) in [("cloudflare-dns.com", cloudflare), ("dns.google", google)] {
            match result {
                Ok(lookup) => {
                    addresses.extend(lookup.addresses);
                    aliases.extend(lookup.aliases);
                }
                Err(error) => {
                    tracing::warn!(%name, resolver, %error, "DoH query failed");
                }
            }
        }
        DohLookup {
            addresses: addresses.into_iter().collect(),
            aliases: aliases.into_iter().collect(),
        }
    }

    async fn query_one(&self, url: &str, name: &str) -> reqwest::Result<DohLookup> {
        // `name` is a hostname, so it needs no percent-encoding; the `query`
        // feature of reqwest is not enabled in this workspace.
        let url = format!("{url}?name={name}&type=A");
        let body = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/dns-json")
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok(parse_lookup(&body))
    }
}

impl Default for DohResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real dns.google response for cache.nixos.org, captured 2026-08-22.
    const CACHE_NIXOS_ORG_RESPONSE: &str = r#"{
        "Status": 0,
        "TC": false,
        "RD": true,
        "RA": true,
        "AD": false,
        "CD": false,
        "Question": [{"name": "cache.nixos.org.", "type": 1}],
        "Answer": [
            {"name": "cache.nixos.org.", "type": 5, "TTL": 20,
             "data": "dualstack.n.sni.global.fastly.net."},
            {"name": "dualstack.n.sni.global.fastly.net.", "type": 1, "TTL": 20,
             "data": "151.101.1.91"},
            {"name": "dualstack.n.sni.global.fastly.net.", "type": 1, "TTL": 20,
             "data": "151.101.65.91"},
            {"name": "dualstack.n.sni.global.fastly.net.", "type": 1, "TTL": 20,
             "data": "151.101.129.91"},
            {"name": "dualstack.n.sni.global.fastly.net.", "type": 1, "TTL": 20,
             "data": "151.101.193.91"}
        ]
    }"#;

    #[test]
    fn parses_a_records_and_cname() {
        let lookup = parse_lookup(CACHE_NIXOS_ORG_RESPONSE);
        assert_eq!(
            lookup.addresses,
            vec![
                Ipv4Addr::new(151, 101, 1, 91),
                Ipv4Addr::new(151, 101, 65, 91),
                Ipv4Addr::new(151, 101, 129, 91),
                Ipv4Addr::new(151, 101, 193, 91),
            ]
        );
        assert_eq!(lookup.aliases, vec!["dualstack.n.sni.global.fastly.net"]);
    }

    #[test]
    fn empty_answer_yields_no_records() {
        assert_eq!(
            parse_lookup(r#"{"Status": 3, "Answer": []}"#),
            DohLookup::default()
        );
        assert_eq!(parse_lookup(r#"{"Status": 3}"#), DohLookup::default());
    }

    #[test]
    fn unparseable_data_is_skipped() {
        let body = r#"{"Answer": [
            {"name": "example.com.", "type": 1, "TTL": 60, "data": "not-an-ip"},
            {"name": "example.com.", "type": 1, "TTL": 60, "data": "192.0.2.1"}
        ]}"#;
        assert_eq!(
            parse_lookup(body).addresses,
            vec![Ipv4Addr::new(192, 0, 2, 1)]
        );
    }

    #[test]
    fn invalid_json_yields_no_records() {
        assert_eq!(parse_lookup("this is not json"), DohLookup::default());
    }

    #[test]
    fn type_one_in_question_section_has_no_data() {
        // The Question section carries `type: 1` without a `data` field;
        // it must not leak into the results.
        let body = r#"{"Question": [{"name": "example.com.", "type": 1}]}"#;
        assert!(parse_lookup(body).addresses.is_empty());
    }
}
