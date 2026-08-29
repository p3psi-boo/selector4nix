use std::net::IpAddr;
use std::num::{NonZeroU64, NonZeroUsize};

use anyhow::{Context, Result as AnyhowResult};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppRawConfiguration {
    pub server: ServerRawConfiguration,
    pub network: Option<NetworkRawConfiguration>,
    pub proxy: Option<ProxyRawConfiguration>,
    pub cache_info: Option<CacheInfoRawConfiguration>,
    pub cache: Option<CacheRawConfiguration>,
    pub substituters: Vec<SubstituterRawConfiguration>,
    pub fastly_optimization: Option<FastlyOptimizationRawConfiguration>,
    pub cloudflare_optimization: Option<CloudflareOptimizationRawConfiguration>,
    pub cloudflare_cache_proxy: Option<CloudflareCacheProxyRawConfiguration>,
}

impl AppRawConfiguration {
    pub fn deserialize(content: &str) -> AnyhowResult<Self> {
        toml::from_str(content).context("could not deserialize content to TOML configuration")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerRawConfiguration {
    pub ip: IpAddr,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct NetworkRawConfiguration {
    pub nar_info_timeout_secs: Option<NonZeroU64>,
    pub nar_timeout_secs: Option<NonZeroU64>,
    pub max_concurrent_requests: Option<NonZeroUsize>,
    pub tolerance_msecs: Option<u64>,
    pub ignore_nar_info_error: Option<bool>,
    pub periodic_probing: Option<bool>,
    pub chunked_streaming: Option<bool>,
    pub streaming_chunk_max_len: Option<NonZeroUsize>,
    pub streaming_window_max_len: Option<NonZeroUsize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ProxyRawConfiguration {
    pub rewrite_nar_url: Option<bool>,
    pub rewrite_to_target: Option<String>,
    pub resolution_policy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CacheInfoRawConfiguration {
    pub store_dir: Option<String>,
    pub want_mass_query: Option<bool>,
    pub priority: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CacheRawConfiguration {
    pub nar_info_cache_capacity: Option<NonZeroUsize>,
    pub nar_info_ttl_secs: Option<NonZeroU64>,
    pub nar_file_cache_capacity: Option<NonZeroUsize>,
    pub nar_file_ttl_secs: Option<NonZeroU64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FastlyOptimizationRawConfiguration {
    pub enabled: Option<bool>,
    pub candidates: Option<Vec<IpAddr>>,
    pub derive_regions: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CloudflareOptimizationRawConfiguration {
    pub enabled: Option<bool>,
    pub candidates: Option<Vec<IpAddr>>,
    pub discovery_domains: Option<Vec<String>>,
}

/// A Cloudflare-hosted reverse proxy that exposes an upstream URL as
/// `/{scheme}/{host}/{path}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CloudflareCacheProxyRawConfiguration {
    pub enabled: Option<bool>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubstituterRawConfiguration {
    pub url: String,
    pub storage_url: Option<String>,
    pub priority: Option<u32>,
    pub nar_info_timeout_secs: Option<NonZeroU64>,
    pub nar_timeout_secs: Option<NonZeroU64>,
    pub max_concurrent_requests: Option<NonZeroUsize>,
}
