use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use selector4nix_core::AppErrorKind;
use selector4nix_core::application::usecase::sni_proxy::{AddSniProxyCommand, AddSniProxyUseCase};
use selector4nix_core::domain::common::url::Url;
use selector4nix_core::domain::substituter::SubstituterRepository;
use selector4nix_core::domain::substituter::model::test_support::make_substituter_normal_with_url;
use selector4nix_core::infrastructure::endpoint::manager::EndpointManager;
use selector4nix_core::infrastructure::endpoint::registry::EndpointManagerRegistry;
use selector4nix_core::infrastructure::provider::{
    EndpointClientPool, EndpointProbingProvider, SniProxySourceProvider,
};
use selector4nix_core::infrastructure::repository::InMemorySubstituterRepository;
use selector4nix_streaming::throttler::{PerHostHttpThrottler, ThrottlingOptions};

fn url(value: &str) -> Url {
    Url::new(value).unwrap()
}

fn ip(octet: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 0, 2, octet))
}

fn make_manager(host: &str) -> Arc<EndpointManager> {
    let pool = Arc::new(EndpointClientPool::new(
        host.to_string(),
        443,
        Arc::new(Client::builder),
        Arc::new(PerHostHttpThrottler::new(ThrottlingOptions::new(
            NonZeroUsize::new(8).unwrap(),
        ))),
        false,
        NonZeroUsize::new(1024).unwrap(),
        NonZeroUsize::new(4096).unwrap(),
        16,
    ));
    let probing = Arc::new(EndpointProbingProvider::new(
        Arc::clone(&pool),
        Duration::from_millis(50),
    ));
    Arc::new(EndpointManager::new(
        host.to_string(),
        Url::new(&format!("https://{host}")).unwrap(),
        pool,
        probing,
        Vec::new(),
        Vec::new(),
        Arc::new(SniProxySourceProvider::new()),
        None,
    ))
}

#[tokio::test]
async fn add_rejects_unknown_substituter() {
    let repository = Arc::new(InMemorySubstituterRepository::new());
    let usecase = AddSniProxyUseCase::new(repository, EndpointManagerRegistry::default());

    let err = usecase
        .run(AddSniProxyCommand {
            substituter_url: url("https://cache.nixos.org/"),
            ip: ip(10),
        })
        .await
        .unwrap_err();

    assert_eq!(err.kind(), AppErrorKind::NotFound);
}

#[tokio::test]
async fn add_rejects_substituter_without_sni_proxy_optimization() {
    let repository = Arc::new(InMemorySubstituterRepository::new());
    let substituter_url = url("https://cache.nixos.org/");
    repository
        .save(make_substituter_normal_with_url(&substituter_url))
        .await;
    let usecase = AddSniProxyUseCase::new(repository, EndpointManagerRegistry::default());

    let err = usecase
        .run(AddSniProxyCommand {
            substituter_url,
            ip: ip(10),
        })
        .await
        .unwrap_err();

    assert_eq!(err.kind(), AppErrorKind::Rule);
    assert!(err.to_string().contains("not enabled"));
}

#[tokio::test]
async fn add_rejects_duplicate_sni_proxy() {
    let repository = Arc::new(InMemorySubstituterRepository::new());
    let substituter_url = url("https://cache.nixos.org/");
    repository
        .save(make_substituter_normal_with_url(&substituter_url))
        .await;
    let manager = make_manager("cache.nixos.org");
    assert!(manager.register_runtime_proxy(ip(10)));
    let usecase = AddSniProxyUseCase::new(repository, EndpointManagerRegistry::new(vec![manager]));

    let err = usecase
        .run(AddSniProxyCommand {
            substituter_url,
            ip: ip(10),
        })
        .await
        .unwrap_err();

    assert_eq!(err.kind(), AppErrorKind::Rule);
    assert!(err.to_string().contains("already exists"));
}
