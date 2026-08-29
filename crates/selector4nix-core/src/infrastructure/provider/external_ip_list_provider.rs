//! Fetching and parsing of externally maintained Cloudflare endpoint lists.

use std::net::IpAddr;
use std::time::Duration;

use dashmap::DashMap;
use futures::StreamExt;
use reqwest::Client;
use tokio::time::Instant;

use crate::infrastructure::config::ExternalIpListConfiguration;

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_LIST_BYTES: usize = 1024 * 1024;
const MAX_LIST_ENTRIES: usize = 4096;

struct CachedIpList {
    endpoints: Vec<IpAddr>,
    refresh_after: Instant,
}

/// Fetches plain-text endpoint lists and keeps the last successful result in
/// memory until its configured refresh interval elapses. A failed refresh uses
/// the stale result, which is still subject to normal TLS and HTTP admission.
pub struct ExternalIpListProvider {
    client: Client,
    cache: DashMap<String, CachedIpList>,
}

impl Default for ExternalIpListProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ExternalIpListProvider {
    pub fn new() -> Self {
        let client = Client::builder()
            .no_proxy()
            .timeout(FETCH_TIMEOUT)
            .build()
            .expect("external IP list client configuration is valid");
        Self {
            client,
            cache: DashMap::new(),
        }
    }

    pub async fn endpoints(&self, config: &ExternalIpListConfiguration) -> Vec<IpAddr> {
        let key = config.url.value().to_string();
        if let Some(cached) = self.cache.get(&key)
            && Instant::now() < cached.refresh_after
        {
            return cached.endpoints.clone();
        }

        match self.fetch(config).await {
            Ok(endpoints) => {
                self.cache.insert(
                    key,
                    CachedIpList {
                        endpoints: endpoints.clone(),
                        refresh_after: Instant::now() + config.refresh_interval,
                    },
                );
                endpoints
            }
            Err(error) => {
                tracing::warn!(url = %config.url, %error, "failed to refresh external endpoint IP list");
                self.cache
                    .get(&key)
                    .map(|cached| cached.endpoints.clone())
                    .unwrap_or_default()
            }
        }
    }

    async fn fetch(&self, config: &ExternalIpListConfiguration) -> anyhow::Result<Vec<IpAddr>> {
        let response = self
            .client
            .get(config.url.value())
            .send()
            .await?
            .error_for_status()?;

        if response
            .content_length()
            .is_some_and(|length| length as usize > MAX_LIST_BYTES)
        {
            return Err(anyhow::anyhow!(
                "external endpoint IP list exceeds {MAX_LIST_BYTES} bytes"
            ));
        }

        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if body.len().saturating_add(chunk.len()) > MAX_LIST_BYTES {
                return Err(anyhow::anyhow!(
                    "external endpoint IP list exceeds {MAX_LIST_BYTES} bytes"
                ));
            }
            body.extend_from_slice(&chunk);
        }

        let content = std::str::from_utf8(&body)
            .map_err(|_| anyhow::anyhow!("external endpoint IP list is not valid UTF-8"))?;
        let endpoints = parse_endpoint_list(content);
        tracing::info!(url = %config.url, endpoints = endpoints.len(), "refreshed external endpoint IP list");
        Ok(endpoints)
    }
}

/// Parse a list with one IP per line. Empty lines and everything after `#`
/// are ignored; domains, CIDRs, and `IP:port` values are deliberately not
/// accepted.
pub(crate) fn parse_endpoint_list(content: &str) -> Vec<IpAddr> {
    let mut endpoints = Vec::new();
    for line in content.lines() {
        let candidate = line.split_once('#').map_or(line, |(value, _)| value).trim();
        if candidate.is_empty() {
            continue;
        }
        let Ok(ip) = candidate.parse::<IpAddr>() else {
            tracing::debug!(
                candidate,
                "skipping invalid external endpoint IP list entry"
            );
            continue;
        };
        if !endpoints.contains(&ip) {
            endpoints.push(ip);
            if endpoints.len() == MAX_LIST_ENTRIES {
                tracing::warn!(
                    limit = MAX_LIST_ENTRIES,
                    "external endpoint IP list was truncated"
                );
                break;
            }
        }
    }
    endpoints
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn parser_supports_blank_lines_and_inline_comments() {
        let endpoints = parse_endpoint_list(
            "\n\
             # full-line comment\n\
             104.16.0.1 # Hong Kong\n\
             2606:4700::6810:1 # IPv6\n\
             invalid.example\n\
             104.16.0.1\n",
        );

        assert_eq!(
            endpoints,
            vec![
                IpAddr::V4(Ipv4Addr::new(104, 16, 0, 1)),
                IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0, 0, 0, 0, 0x6810, 1)),
            ]
        );
    }

    #[test]
    fn parser_rejects_domains_cidrs_and_ports() {
        assert!(parse_endpoint_list("example.com\n1.2.3.4/24\n1.2.3.4:443\n").is_empty());
    }
}
