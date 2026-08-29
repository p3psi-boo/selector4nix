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
    assert!(!config.cloudflare_cache_proxy.enabled);
    assert!(config.cloudflare_cache_proxy.url.is_none());
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
    assert!(!config.fastly_optimization.derive_regions);
}

#[test]
fn fastly_optimization_is_parsed_when_enabled() {
    let config = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[fastly_optimization]
enabled = true
candidates = ["151.101.1.91", "151.101.65.91"]
derive_regions = true
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
    assert!(config.fastly_optimization.derive_regions);
}

#[test]
fn fastly_optimization_requires_cache_nixos_org_substituter() {
    let result = AppConfiguration::deserialize(
        r#"
[server]
ip = "127.0.0.1"

[[substituters]]
url = "https://mirror.example.com/"

[fastly_optimization]
enabled = true
"#,
    );

    assert!(result.is_err());
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
    assert_eq!(
        config.cloudflare_optimization.discovery_domains,
        vec!["cloudflare.182682.xyz".to_string()],
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
discovery_domains = ["cf.example.com"]
"#,
    ))
    .unwrap();

    assert!(config.cloudflare_optimization.enabled);
    assert_eq!(
        config.cloudflare_optimization.candidates,
        vec!["1.2.3.4".parse::<std::net::IpAddr>().unwrap()],
    );
    assert_eq!(
        config.cloudflare_optimization.discovery_domains,
        vec!["cf.example.com".to_string()],
    );
}

#[test]
fn cloudflare_optimization_requires_cachix_substituter() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[cloudflare_optimization]
enabled = true
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn cloudflare_optimization_explicit_empty_discovery_domains_stays_empty() {
    let config = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[cloudflare_optimization]
discovery_domains = []
"#,
    ))
    .unwrap();

    assert!(config.cloudflare_optimization.discovery_domains.is_empty());
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
fn cloudflare_cache_proxy_routes_cache_nixos_org_through_proxy() {
    let config = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[cloudflare_cache_proxy]
enabled = true
url = "https://reverse-proxy.example/"

[cloudflare_optimization]
enabled = true
"#,
    ))
    .unwrap();

    assert!(config.cloudflare_cache_proxy.enabled);
    assert_eq!(
        config.cloudflare_cache_proxy.url.unwrap().value(),
        "https://reverse-proxy.example/"
    );
    assert_eq!(
        config.substituters[0].url.value(),
        "https://reverse-proxy.example/https/cache.nixos.org/"
    );
    assert_eq!(
        config.substituters[0]
            .url
            .as_dir()
            .join("nar/example.nar.xz")
            .unwrap()
            .value(),
        "https://reverse-proxy.example/https/cache.nixos.org/nar/example.nar.xz"
    );
}

#[test]
fn cloudflare_cache_proxy_requires_url_when_enabled() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[cloudflare_cache_proxy]
enabled = true
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn cloudflare_cache_proxy_requires_https_url() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[cloudflare_cache_proxy]
url = "http://download.example.com/"
"#,
    ));

    assert!(result.is_err());
}

#[test]
fn cloudflare_cache_proxy_requires_cache_nixos_org_substituter() {
    let result = AppConfiguration::deserialize(
        r#"
[server]
ip = "127.0.0.1"

[[substituters]]
url = "https://mirror.example.com/"

[cloudflare_cache_proxy]
enabled = true
url = "https://download.example.com/"
"#,
    );

    assert!(result.is_err());
}

#[test]
fn fastly_and_cloudflare_cache_proxy_are_mutually_exclusive() {
    let result = AppConfiguration::deserialize(&make_config_string_overriden(
        r#"
[fastly_optimization]
enabled = true

[cloudflare_cache_proxy]
enabled = true
url = "https://download.example.com/"
"#,
    ));

    assert!(result.is_err());
}
