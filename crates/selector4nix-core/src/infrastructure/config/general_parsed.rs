use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Error as AnyhowError, Result as AnyhowResult};

use crate::domain::common::url::Url;
use crate::domain::nar_info::ResolutionPolicyOption;
use crate::domain::nar_info::model::NarUrlRewriteOption;
use crate::domain::substituter::model::{PeriodicProbingOption, Priority};
use crate::infrastructure::config::general_raw::{
    AppRawConfiguration, BandwidthProbeRawConfiguration, CacheInfoRawConfiguration,
    CacheRawConfiguration, CloudflareOptimizationRawConfiguration,
    FastlyOptimizationRawConfiguration, NetworkRawConfiguration, ProxyRawConfiguration,
    ServerRawConfiguration, SniProxySourceRawConfiguration, SubstituterRawConfiguration,
};

const DEFAULT_FASTLY_BANDWIDTH_PROBE_URL: &str =
    "https://cache.nixos.org/nar/1as5cn000kck2y35awm3825qvlvcnq08jbilwdlzdlv6pbidk3i4.nar.zst";
const DEFAULT_BANDWIDTH_PROBE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppConfiguration {
    pub server: ServerConfiguration,
    pub network: NetworkConfiguration,
    pub proxy: ProxyConfiguration,
    pub cache_info: CacheInfoConfiguration,
    pub cache: CacheConfiguration,
    pub substituters: Vec<SubstituterConfiguration>,
    pub fastly_optimization: FastlyOptimizationConfiguration,
    pub cloudflare_optimization: CloudflareOptimizationConfiguration,
}

impl AppConfiguration {
    pub fn deserialize(content: &str) -> AnyhowResult<Self> {
        AppRawConfiguration::deserialize(content)?
            .try_into()
            .context("configuration contains invalid value")
    }

    pub fn load_from(path: &Path) -> AnyhowResult<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("could not read configuration from {}", path.display()))?;
        let configuration = Self::deserialize(&content)?;
        tracing::info!(path = %path.display(), "loaded configuration");
        Ok(configuration)
    }

    pub fn load() -> AnyhowResult<Self> {
        let path = if let Ok(path) = std::env::var("SELECTOR4NIX_CONFIG_FILE") {
            tracing::info!(path = %path, "use configuration file from environment variable");
            PathBuf::from(path)
        } else if let Ok(path) = Path::new("./selector4nix.toml").canonicalize() {
            tracing::info!(path = %path.display(), "use configuration file from current directory");
            path
        } else if let Ok(path) = Path::new("/etc/selector4nix/selector4nix.toml").canonicalize() {
            tracing::info!(path = %path.display(), "use configuration file from `/etc`");
            path
        } else {
            tracing::error!("could not find any configuration file");
            return Err(anyhow::anyhow!("could not find any configuration file"));
        };

        Self::load_from(&path)
    }
}

impl TryFrom<AppRawConfiguration> for AppConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: AppRawConfiguration) -> Result<Self, Self::Error> {
        let AppRawConfiguration {
            server,
            network,
            proxy,
            cache_info,
            cache,
            substituters: raw_substituters,
            fastly_optimization: raw_fastly_optimization,
            cloudflare_optimization: raw_cloudflare_optimization,
        } = raw;

        if raw_substituters.is_empty() {
            return Err(anyhow::anyhow!(
                "at least one substituter must be configured"
            ));
        }
        let substituters = raw_substituters
            .into_iter()
            .map(|c| c.try_into())
            .collect::<Result<Vec<SubstituterConfiguration>, _>>()?;
        let fastly_optimization: FastlyOptimizationConfiguration =
            raw_fastly_optimization.unwrap_or_default().try_into()?;

        let cloudflare_optimization: CloudflareOptimizationConfiguration =
            raw_cloudflare_optimization.unwrap_or_default().try_into()?;
        Ok(Self {
            server: server.try_into()?,
            network: network.unwrap_or_default().try_into()?,
            proxy: proxy.unwrap_or_default().try_into()?,
            cache_info: cache_info.unwrap_or_default().try_into()?,
            cache: cache.unwrap_or_default().try_into()?,
            substituters,
            fastly_optimization,
            cloudflare_optimization,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerConfiguration {
    pub ip: IpAddr,
    pub port: u16,
}

impl ServerConfiguration {
    pub fn listen_addr(&self) -> SocketAddr {
        SocketAddr::new(self.ip, self.port)
    }
}

impl TryFrom<ServerRawConfiguration> for ServerConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: ServerRawConfiguration) -> Result<Self, Self::Error> {
        Ok(Self {
            ip: raw.ip,
            port: raw.port.unwrap_or(5496),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetworkConfiguration {
    pub nar_info_timeout: Duration,
    pub nar_timeout: Duration,
    pub max_concurrent_requests: NonZeroUsize,
    pub tolerance: u64,
    pub ignore_nar_info_error: bool,
    pub periodic_probing: PeriodicProbingOption,
    pub chunked_streaming: bool,
    pub streaming_chunk_max_len: NonZeroUsize,
    pub streaming_window_max_len: NonZeroUsize,
}

impl TryFrom<NetworkRawConfiguration> for NetworkConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: NetworkRawConfiguration) -> Result<Self, Self::Error> {
        Ok(Self {
            nar_info_timeout: raw
                .nar_info_timeout_secs
                .map_or(Duration::from_secs(30), |s| Duration::from_secs(s.get())),
            nar_timeout: raw
                .nar_timeout_secs
                .map_or(Duration::from_secs(30), |s| Duration::from_secs(s.get())),
            max_concurrent_requests: raw
                .max_concurrent_requests
                .unwrap_or(NonZeroUsize::new(12).unwrap()),
            tolerance: raw.tolerance_msecs.unwrap_or(50),
            ignore_nar_info_error: raw.ignore_nar_info_error.unwrap_or(false),
            periodic_probing: if raw.periodic_probing.unwrap_or(true) {
                PeriodicProbingOption::Enabled
            } else {
                PeriodicProbingOption::None
            },
            chunked_streaming: raw.chunked_streaming.unwrap_or(true),
            streaming_chunk_max_len: raw
                .streaming_chunk_max_len
                .unwrap_or(NonZeroUsize::new(4 * 1024 * 1024).unwrap()),
            streaming_window_max_len: raw
                .streaming_window_max_len
                .unwrap_or(NonZeroUsize::new(8).unwrap()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProxyConfiguration {
    pub rewrite_nar_url: NarUrlRewriteOption,
    pub resolution_policy: ResolutionPolicyOption,
}

impl TryFrom<ProxyRawConfiguration> for ProxyConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: ProxyRawConfiguration) -> Result<Self, Self::Error> {
        Ok(Self {
            rewrite_nar_url: if raw.rewrite_nar_url.unwrap_or(true) {
                match raw.rewrite_to_target.unwrap_or("self".into()).as_str() {
                    "self" => NarUrlRewriteOption::ToSelf,
                    "upstream" => NarUrlRewriteOption::ToUpstream,
                    _ => {
                        return Err(anyhow::anyhow!(
                            "`proxy.rewrite_to_target` should be `\"self\"` or `\"upstream\"`"
                        ));
                    }
                }
            } else {
                NarUrlRewriteOption::Keep
            },
            resolution_policy: raw.resolution_policy.map_or(
                Ok(ResolutionPolicyOption::Preference),
                |value| match value.as_str() {
                    "preference" => Ok(ResolutionPolicyOption::Preference),
                    "tier" => Ok(ResolutionPolicyOption::Tier),
                    _ => Err(anyhow::anyhow!(
                        "`proxy.resolution_policy` should be `\"preference\"` or `\"tier\"`"
                    )),
                },
            )?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheInfoConfiguration {
    pub store_dir: String,
    pub want_mass_query: bool,
    pub priority: Priority,
}

impl TryFrom<CacheInfoRawConfiguration> for CacheInfoConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: CacheInfoRawConfiguration) -> Result<Self, Self::Error> {
        Ok(Self {
            store_dir: raw.store_dir.map_or(Ok("/nix/store".into()), |s| {
                if s.starts_with("/") {
                    Ok(s)
                } else {
                    Err(anyhow::anyhow!(
                        "config `cache.store_dir` should be an absolute path"
                    ))
                }
            })?,
            want_mass_query: raw.want_mass_query.unwrap_or(true),
            priority: raw.priority.map_or(Priority::new(40), Priority::new)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheConfiguration {
    pub nar_info_cache_capacity: NonZeroUsize,
    pub nar_info_ttl: Duration,
    pub nar_file_cache_capacity: NonZeroUsize,
    pub nar_file_ttl: Duration,
}

impl TryFrom<CacheRawConfiguration> for CacheConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: CacheRawConfiguration) -> Result<Self, Self::Error> {
        Ok(Self {
            nar_info_cache_capacity: raw
                .nar_info_cache_capacity
                .unwrap_or(NonZeroUsize::new(4096).unwrap()),
            nar_info_ttl: raw
                .nar_info_ttl_secs
                .map_or(Duration::from_hours(4), |s| Duration::from_secs(s.get())),
            nar_file_cache_capacity: raw
                .nar_file_cache_capacity
                .unwrap_or(NonZeroUsize::new(4096).unwrap()),
            nar_file_ttl: raw
                .nar_file_ttl_secs
                .map_or(Duration::from_hours(4), |s| Duration::from_secs(s.get())),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FastlyOptimizationConfiguration {
    pub enabled: bool,
    pub candidates: Vec<IpAddr>,
    pub derive_regions: bool,
    pub sni_proxy_sources: Vec<SniProxySourceConfiguration>,
    pub bandwidth_probe: BandwidthProbeConfiguration,
}

impl Default for FastlyOptimizationConfiguration {
    fn default() -> Self {
        Self {
            enabled: false,
            candidates: Vec::new(),
            derive_regions: false,
            sni_proxy_sources: Vec::new(),
            bandwidth_probe: BandwidthProbeConfiguration::fastly_default(),
        }
    }
}

impl TryFrom<FastlyOptimizationRawConfiguration> for FastlyOptimizationConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: FastlyOptimizationRawConfiguration) -> Result<Self, Self::Error> {
        let FastlyOptimizationRawConfiguration {
            enabled,
            candidates: raw_candidates,
            derive_regions,
            sni_proxy_sources,
            bandwidth_probe,
        } = raw;
        let mut candidates = Vec::new();
        for candidate in raw_candidates.unwrap_or_default() {
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
        Ok(Self {
            enabled: enabled.unwrap_or(false),
            candidates,
            derive_regions: derive_regions.unwrap_or(false),
            sni_proxy_sources: parse_sni_proxy_sources(sni_proxy_sources)?,
            bandwidth_probe: BandwidthProbeConfiguration::for_fastly(
                bandwidth_probe.unwrap_or_default(),
            )?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CloudflareOptimizationConfiguration {
    pub enabled: bool,
    pub candidates: Vec<IpAddr>,
    pub discovery_domains: Vec<String>,
    pub sni_proxy_sources: Vec<SniProxySourceConfiguration>,
    pub bandwidth_probe: BandwidthProbeConfiguration,
}

impl Default for CloudflareOptimizationConfiguration {
    fn default() -> Self {
        Self {
            enabled: false,
            candidates: Vec::new(),
            discovery_domains: default_cloudflare_discovery_domains(),
            sni_proxy_sources: Vec::new(),
            bandwidth_probe: BandwidthProbeConfiguration::cloudflare_default(),
        }
    }
}

fn default_cloudflare_discovery_domains() -> Vec<String> {
    vec!["cloudflare.182682.xyz".to_string()]
}

impl TryFrom<CloudflareOptimizationRawConfiguration> for CloudflareOptimizationConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: CloudflareOptimizationRawConfiguration) -> Result<Self, Self::Error> {
        let CloudflareOptimizationRawConfiguration {
            enabled,
            candidates: raw_candidates,
            discovery_domains,
            sni_proxy_sources,
            bandwidth_probe,
        } = raw;
        let mut candidates = Vec::new();
        for candidate in raw_candidates.unwrap_or_default() {
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
        Ok(Self {
            enabled: enabled.unwrap_or(false),
            candidates,
            discovery_domains: discovery_domains
                .unwrap_or_else(default_cloudflare_discovery_domains),
            sni_proxy_sources: parse_sni_proxy_sources(sni_proxy_sources)?,
            bandwidth_probe: BandwidthProbeConfiguration::for_cloudflare(
                bandwidth_probe.unwrap_or_default(),
            )?,
        })
    }
}

/// A `file://`, `http://`, or `https://` plain-text list of SNI proxy IPs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SniProxySourceConfiguration {
    pub url: url::Url,
    pub refresh_interval: Duration,
}

impl TryFrom<SniProxySourceRawConfiguration> for SniProxySourceConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: SniProxySourceRawConfiguration) -> Result<Self, Self::Error> {
        let url = url::Url::parse(&raw.url)
            .with_context(|| format!("invalid SNI proxy source URL `{}`", raw.url))?;
        match url.scheme() {
            "file" => {
                if url.query().is_some() || url.fragment().is_some() || url.to_file_path().is_err()
                {
                    return Err(anyhow::anyhow!(
                        "SNI proxy `file://` source must be an absolute file URL without query or fragment"
                    ));
                }
            }
            "http" | "https" if url.host_str().is_some() => {}
            _ => {
                return Err(anyhow::anyhow!(
                    "SNI proxy source URL must use `file`, `http`, or `https`"
                ));
            }
        }
        Ok(Self {
            url,
            refresh_interval: raw
                .refresh_secs
                .map_or(Duration::from_secs(60 * 60), |seconds| {
                    Duration::from_secs(seconds.get())
                }),
        })
    }
}

fn parse_sni_proxy_sources(
    raw_sources: Option<Vec<SniProxySourceRawConfiguration>>,
) -> AnyhowResult<Vec<SniProxySourceConfiguration>> {
    let mut sources = Vec::new();
    for source in raw_sources.unwrap_or_default() {
        let source = source.try_into()?;
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    Ok(sources)
}

/// Active bounded-download benchmark configuration shared by Fastly and
/// Cloudflare endpoint managers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BandwidthProbeConfiguration {
    pub enabled: bool,
    pub url: Url,
    pub bytes: NonZeroUsize,
    pub refresh_interval: Duration,
    pub max_concurrent_probes: NonZeroUsize,
}

impl BandwidthProbeConfiguration {
    fn fastly_default() -> Self {
        Self::with_default_url(DEFAULT_FASTLY_BANDWIDTH_PROBE_URL)
    }

    fn cloudflare_default() -> Self {
        Self::with_default_url(&default_cloudflare_bandwidth_probe_url(
            DEFAULT_BANDWIDTH_PROBE_BYTES,
        ))
    }

    fn with_default_url(url: &str) -> Self {
        Self {
            enabled: true,
            url: Url::new(url).expect("default bandwidth probe URL is valid"),
            bytes: NonZeroUsize::new(DEFAULT_BANDWIDTH_PROBE_BYTES).expect("10 MiB is non-zero"),
            refresh_interval: Duration::from_secs(6 * 60 * 60),
            max_concurrent_probes: NonZeroUsize::new(2).expect("2 is non-zero"),
        }
    }

    fn for_fastly(raw: BandwidthProbeRawConfiguration) -> AnyhowResult<Self> {
        Self::from_raw(raw, |_: usize| {
            DEFAULT_FASTLY_BANDWIDTH_PROBE_URL.to_string()
        })
    }

    fn for_cloudflare(raw: BandwidthProbeRawConfiguration) -> AnyhowResult<Self> {
        Self::from_raw(raw, default_cloudflare_bandwidth_probe_url)
    }

    fn from_raw(
        raw: BandwidthProbeRawConfiguration,
        default_url: impl FnOnce(usize) -> String,
    ) -> AnyhowResult<Self> {
        let bytes = raw
            .bytes
            .unwrap_or(NonZeroUsize::new(DEFAULT_BANDWIDTH_PROBE_BYTES).unwrap());
        let url = Url::new(&raw.url.unwrap_or_else(|| default_url(bytes.get())))?;
        if url.inner().scheme() != "https" {
            return Err(anyhow::anyhow!(
                "bandwidth probe URL must use HTTPS so SNI and certificate validation are tested"
            ));
        }
        Ok(Self {
            enabled: raw.enabled.unwrap_or(true),
            url,
            bytes,
            refresh_interval: raw
                .refresh_secs
                .map_or(Duration::from_secs(6 * 60 * 60), |seconds| {
                    Duration::from_secs(seconds.get())
                }),
            max_concurrent_probes: raw
                .max_concurrent_probes
                .unwrap_or(NonZeroUsize::new(2).unwrap()),
        })
    }
}

fn default_cloudflare_bandwidth_probe_url(bytes: usize) -> String {
    format!("https://speed.cloudflare.com/__down?bytes={bytes}")
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubstituterConfiguration {
    pub url: Url,
    pub storage_url: Option<Url>,
    pub priority: Priority,
    pub nar_info_timeout: Option<Duration>,
    pub nar_timeout: Option<Duration>,
    pub max_concurrent_requests: Option<NonZeroUsize>,
}

impl TryFrom<SubstituterRawConfiguration> for SubstituterConfiguration {
    type Error = AnyhowError;

    fn try_from(raw: SubstituterRawConfiguration) -> Result<Self, Self::Error> {
        Ok(Self {
            url: Url::new(&raw.url)?,
            storage_url: raw.storage_url.map(|s| Url::new(&s)).transpose()?,
            priority: raw.priority.map_or(Priority::new(40), Priority::new)?,
            nar_info_timeout: raw
                .nar_info_timeout_secs
                .map(|s| Duration::from_secs(s.get())),
            nar_timeout: raw.nar_timeout_secs.map(|s| Duration::from_secs(s.get())),
            max_concurrent_requests: raw.max_concurrent_requests,
        })
    }
}
