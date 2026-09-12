use std::num::NonZeroUsize;
use std::time::Duration;

use selector4nix_core::domain::nar_info::ResolutionPolicyOption;
use selector4nix_core::domain::nar_info::model::NarUrlRewriteOption;
use selector4nix_core::domain::substituter::model::PeriodicProbingOption;
use selector4nix_core::infrastructure::config::AppConfiguration;

use super::fixture::config::{make_config_string_minimal, make_config_string_overriden};

#[test]
fn example_config_file_is_valid() {
    let content = include_str!("../../../../docs/selector4nix.example.toml");
    AppConfiguration::deserialize(content).unwrap();
}

#[test]
fn defaults_are_applied_when_sections_omitted() {
    let config = AppConfiguration::deserialize(&make_config_string_minimal()).unwrap();

    assert_eq!(config.server.port, 5496);
    assert_eq!(config.network.nar_info_timeout, Duration::from_secs(30));
    assert_eq!(config.network.nar_timeout, Duration::from_secs(30));
    assert_eq!(
        config.network.max_concurrent_requests,
        NonZeroUsize::new(12).unwrap(),
    );
    assert_eq!(config.network.tolerance, 50);
    assert!(!config.network.ignore_nar_info_error);
    assert_eq!(
        config.network.periodic_probing,
        PeriodicProbingOption::Enabled,
    );
    assert!(config.network.chunked_streaming);
    assert_eq!(
        config.network.streaming_chunk_max_len,
        NonZeroUsize::new(4 * 1024 * 1024).unwrap(),
    );
    assert_eq!(
        config.network.streaming_window_max_len,
        NonZeroUsize::new(8).unwrap(),
    );
    assert_eq!(config.proxy.rewrite_nar_url, NarUrlRewriteOption::ToSelf);
    assert_eq!(
        config.proxy.resolution_policy,
        ResolutionPolicyOption::Preference,
    );
    assert_eq!(config.cache_info.store_dir, "/nix/store");
    assert!(config.cache_info.want_mass_query);
    assert_eq!(config.cache_info.priority.value(), 40);
    assert_eq!(
        config.cache.nar_info_cache_capacity,
        NonZeroUsize::new(4096).unwrap(),
    );
    assert_eq!(config.cache.nar_info_ttl, Duration::from_secs(14400));
    assert_eq!(
        config.cache.nar_file_cache_capacity,
        NonZeroUsize::new(4096).unwrap(),
    );
    assert_eq!(config.cache.nar_file_ttl, Duration::from_secs(14400));
    assert_eq!(config.substituters.len(), 1);
    assert!(config.substituters[0].storage_url.is_none());
    assert!(config.substituters[0].nar_info_timeout.is_none());
    assert!(config.substituters[0].nar_timeout.is_none());
    assert!(config.substituters[0].max_concurrent_requests.is_none());
}

#[test]
fn invalid_rewrite_to_target_is_rejected() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[proxy]
rewrite_to_target = "invalid"
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn invalid_resolution_policy_is_rejected() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[proxy]
resolution_policy = "invalid"
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn non_absolute_store_dir_is_rejected() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[cache_info]
store_dir = "relative/path"
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn zero_priority_is_rejected() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[[substituters]]
url = "https://cache.nixos.org/"
priority = 0
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn empty_substituters_is_rejected() {
    let result = AppConfiguration::deserialize(
        r#"
[server]
ip = "127.0.0.1"
"#,
    );

    assert!(result.is_err());
}

#[test]
fn fastly_optimization_defaults_to_disabled() {
    let config = AppConfiguration::deserialize(&make_config_string_minimal()).unwrap();

    assert!(!config.fastly_optimization.enabled);
    assert!(config.fastly_optimization.candidates.is_empty());
    assert!(config.fastly_optimization.sni_proxy_sources.is_empty());
    assert!(config.fastly_optimization.bandwidth_probe.enabled);
}

#[test]
fn fastly_optimization_is_parsed_when_enabled() {
    let config = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[fastly_optimization]
enabled = true
candidates = ["151.101.1.91", "151.101.65.91"]
sni_proxy_sources = [
  { url = "file:///tmp/fastly-sni-proxies.txt", refresh_secs = 60 },
]

[fastly_optimization.bandwidth_probe]
url = "https://cache.nixos.org/nar/probe.nar.zst"
bytes = 1048576
"#,
    ))
    .unwrap();

    assert!(config.fastly_optimization.enabled);
    assert_eq!(
        config.fastly_optimization.candidates,
        vec![
            "151.101.1.91".parse::<std::net::IpAddr>().unwrap(),
            "151.101.65.91".parse::<std::net::IpAddr>().unwrap(),
        ],
    );
    assert_eq!(config.fastly_optimization.sni_proxy_sources.len(), 1);
    assert_eq!(
        config.fastly_optimization.bandwidth_probe.url.value(),
        "https://cache.nixos.org/nar/probe.nar.zst"
    );
    assert_eq!(
        config.fastly_optimization.bandwidth_probe.bytes.get(),
        1048576
    );
}

#[test]
fn fastly_optimization_accepts_runtime_detected_substituter_hosts() {
    let config = AppConfiguration::deserialize(
        r#"
[server]
ip = "127.0.0.1"

[[substituters]]
url = "https://mirror.example.com/"

[fastly_optimization]
enabled = true
"#,
    )
    .unwrap();

    assert!(config.fastly_optimization.enabled);
    assert_eq!(config.substituters[0].url.host(), "mirror.example.com");
}

#[test]
fn fastly_optimization_candidates_are_deduplicated() {
    let config = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[fastly_optimization]
candidates = ["151.101.1.91", "151.101.65.91", "151.101.1.91"]
"#,
    ))
    .unwrap();

    assert_eq!(
        config.fastly_optimization.candidates,
        vec![
            "151.101.1.91".parse::<std::net::IpAddr>().unwrap(),
            "151.101.65.91".parse::<std::net::IpAddr>().unwrap(),
        ],
    );
}

#[test]
fn fastly_optimization_rejects_non_ip_candidates() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[fastly_optimization]
candidates = ["cache.nixos.org"]
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn cloudflare_optimization_defaults_to_disabled() {
    let config = AppConfiguration::deserialize(&make_config_string_minimal()).unwrap();

    assert!(!config.cloudflare_optimization.enabled);
    assert!(config.cloudflare_optimization.candidates.is_empty());
    assert!(config.cloudflare_optimization.sni_proxy_sources.is_empty());
    assert!(config.cloudflare_optimization.bandwidth_probe.enabled);
    assert_eq!(
        config.cloudflare_optimization.bandwidth_probe.url.value(),
        "https://speed.cloudflare.com/__down?bytes=1048576"
    );
    assert_eq!(
        config.cloudflare_optimization.bandwidth_probe.bytes.get(),
        1048576
    );
    assert_eq!(
        config
            .cloudflare_optimization
            .bandwidth_probe
            .refresh_interval,
        Duration::from_secs(86400)
    );
    assert_eq!(
        config
            .cloudflare_optimization
            .bandwidth_probe
            .max_concurrent_probes
            .get(),
        1
    );
}

#[test]
fn cloudflare_optimization_is_parsed_when_enabled() {
    let config = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[[substituters]]
url = "https://nix-community.cachix.org/"

[cloudflare_optimization]
enabled = true
candidates = ["1.2.3.4"]
sni_proxy_sources = [
  { url = "http://ips.example/cloudflare.txt", refresh_secs = 900 },
  { url = "file:///tmp/cloudflare-sni-proxies.txt" },
]

[cloudflare_optimization.bandwidth_probe]
url = "https://speed.cloudflare.com/__down?bytes=2097152"
bytes = 2097152
"#,
    ))
    .unwrap();

    assert!(config.cloudflare_optimization.enabled);
    assert_eq!(
        config.cloudflare_optimization.candidates,
        vec!["1.2.3.4".parse::<std::net::IpAddr>().unwrap()],
    );
    assert_eq!(config.cloudflare_optimization.sni_proxy_sources.len(), 2);
    assert_eq!(
        config.cloudflare_optimization.sni_proxy_sources[0]
            .url
            .as_str(),
        "http://ips.example/cloudflare.txt"
    );
    assert_eq!(
        config.cloudflare_optimization.sni_proxy_sources[0].refresh_interval,
        Duration::from_secs(900)
    );
    assert_eq!(
        config.cloudflare_optimization.bandwidth_probe.bytes.get(),
        2097152
    );
}

#[test]
fn cloudflare_optimization_accepts_runtime_detected_substituter_hosts() {
    let config = AppConfiguration::deserialize(
        r#"
[server]
ip = "127.0.0.1"

[[substituters]]
url = "https://mirror.example.com/"

[cloudflare_optimization]
enabled = true
"#,
    )
    .unwrap();

    assert!(config.cloudflare_optimization.enabled);
    assert_eq!(config.substituters[0].url.host(), "mirror.example.com");
}

#[test]
fn dns_endpoint_discovery_fields_are_rejected() {
    let derive_regions = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[fastly_optimization]
derive_regions = true
"#,
    ));
    let discovery_domains = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[cloudflare_optimization]
discovery_domains = ["cloudflare.182682.xyz"]
"#,
    ));

    assert!(derive_regions.is_err());
    assert!(discovery_domains.is_err());
}

#[test]
fn cloudflare_optimization_candidates_are_deduplicated() {
    let config = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[cloudflare_optimization]
candidates = ["1.2.3.4", "5.6.7.8", "1.2.3.4"]
"#,
    ))
    .unwrap();

    assert_eq!(
        config.cloudflare_optimization.candidates,
        vec![
            "1.2.3.4".parse::<std::net::IpAddr>().unwrap(),
            "5.6.7.8".parse::<std::net::IpAddr>().unwrap(),
        ],
    );
}

#[test]
fn sni_proxy_sources_accept_file_http_and_https() {
    let config = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[fastly_optimization]
sni_proxy_sources = [
  { url = "file:///tmp/fastly.txt" },
  { url = "http://lists.example/fastly.txt" },
  { url = "https://lists.example/fastly.txt" },
]
"#,
    ))
    .unwrap();

    assert_eq!(config.fastly_optimization.sni_proxy_sources.len(), 3);
}

#[test]
fn sni_proxy_sources_reject_unsupported_schemes() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[fastly_optimization]
sni_proxy_sources = [{ url = "ftp://lists.example/fastly.txt" }]
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn cloudflare_http_reverse_proxy_configuration_was_removed() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[cloudflare_cache_proxy]
enabled = true
url = "https://reverse-proxy.example/"
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn fastly_and_cloudflare_keep_separate_sni_proxy_sources() {
    let config = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[[substituters]]
url = "https://nix-community.cachix.org/"

[fastly_optimization]
enabled = true
sni_proxy_sources = [{ url = "file:///tmp/fastly.txt" }]

[cloudflare_optimization]
enabled = true
sni_proxy_sources = [{ url = "file:///tmp/cloudflare.txt" }]
"#,
    ))
    .unwrap();

    assert_eq!(
        config.fastly_optimization.sni_proxy_sources[0].url.as_str(),
        "file:///tmp/fastly.txt"
    );
    assert_eq!(
        config.cloudflare_optimization.sni_proxy_sources[0]
            .url
            .as_str(),
        "file:///tmp/cloudflare.txt"
    );
}

#[test]
fn bandwidth_probe_requires_https() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[fastly_optimization.bandwidth_probe]
url = "http://cache.nixos.org/nar/probe.nar.zst"
"#,
    ));

    assert!(result.is_err());
}
