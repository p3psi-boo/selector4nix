use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result as AnyhowResult};
use redb::Database;
use redb::backends::InMemoryBackend;
use reqwest::{Client, ClientBuilder};
use selector4nix_actor::registry::{
    AsyncFactory, CapacityOption, ExpirationOption, RegistryBuilder,
};
use selector4nix_core::AppContext;
use selector4nix_core::application::actor::nar_file::NarFileActor;
use selector4nix_core::application::actor::nar_info::NarInfoActor;
use selector4nix_core::application::actor::substituter::SubstituterActor;
use selector4nix_core::application::usecase::dashboard::{
    GetDashboardCacheStatsUseCase, GetDashboardConfigSummaryUseCase,
};
use selector4nix_core::application::usecase::dashboard::{
    GetDashboardOverviewUseCase, GetDashboardTransferringUseCase,
};
use selector4nix_core::application::usecase::derivation::GetDerivationLogUseCase;
use selector4nix_core::application::usecase::nar_file::StreamNarFileUseCase;
use selector4nix_core::application::usecase::nar_info::{
    ListNarInnerDirectoryUseCase, ResolveNarInfoUseCase,
};
use selector4nix_core::application::usecase::sni_proxy::AddSniProxyUseCase;
use selector4nix_core::application::usecase::substituter::{
    AddSubstituterUseCase, DisableSubstituterUseCase, EnableSubstituterUseCase,
};
use selector4nix_core::domain::common::passthrough_headers::SELF_USER_AGENT;
use selector4nix_core::domain::common::url::Url;
use selector4nix_core::domain::nar_file::NarFileService;
use selector4nix_core::domain::nar_file::model::NarFileKey;
use selector4nix_core::domain::nar_info::model::StorePathHash;
use selector4nix_core::domain::nar_info::policy::{PreferencePolicy, TierPolicy};
use selector4nix_core::domain::nar_info::{
    NarInfoResolutionPolicy, NarInfoService, ResolutionPolicyOption,
};
use selector4nix_core::domain::substituter::model::{
    Availability, EndpointOptimizationKind, Substituter, SubstituterMeta,
};
use selector4nix_core::domain::substituter::{SubstituterRepository, SubstituterService};
use selector4nix_core::infrastructure::cdn::CdnDetector;
use selector4nix_core::infrastructure::config::{AppConfiguration, AppCredential};
use selector4nix_core::infrastructure::dns::doh_resolver::DohResolver;
use selector4nix_core::infrastructure::endpoint::manager::EndpointManager;
use selector4nix_core::infrastructure::endpoint::registry::EndpointManagerRegistry;
use selector4nix_core::infrastructure::metric::NarTransferMetric;
use selector4nix_core::infrastructure::provider::*;
use selector4nix_core::infrastructure::repository::*;
use selector4nix_db::cache_kv::CacheKv;
use selector4nix_streaming::throttler::PerHostHttpThrottler;
use selector4nix_streaming::{StreamingClient, ThrottlingOptions};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, Registry};

use crate::cli::LogLevel;

const ENDPOINT_CLIENT_POOL_CAPACITY: usize = 16;
const ENDPOINT_REFRESH_INTERVAL: Duration = Duration::from_secs(30 * 60);
const ENDPOINT_REFRESH_MAX_JITTER: Duration = Duration::from_secs(300);

pub fn init_logger(
    log_file: Option<PathBuf>,
    log_level: Option<LogLevel>,
    no_timestamp: bool,
) -> AnyhowResult<()> {
    let registry = tracing_subscriber::registry();

    let writer = if let Some(file) = log_file {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .with_context(|| format!("could not open log file: {}", file.display()))?;
        Some(Arc::new(file))
    } else {
        None
    };

    let filter = if let Some(level) = log_level {
        EnvFilter::new(level.to_string())
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };

    let registry = {
        let fmt_layer: Box<dyn Layer<Registry> + Send + Sync> = match (no_timestamp, writer) {
            (true, Some(writer)) => Box::new(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_writer(writer)
                    .with_ansi(false)
                    .with_filter(filter.clone()),
            ),
            (false, Some(writer)) => Box::new(
                tracing_subscriber::fmt::layer()
                    .with_writer(writer)
                    .with_ansi(false)
                    .with_filter(filter.clone()),
            ),
            (true, None) => Box::new(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_filter(filter.clone()),
            ),
            (false, None) => Box::new(tracing_subscriber::fmt::layer().with_filter(filter.clone())),
        };
        registry.with(fmt_layer)
    };

    #[cfg(all(target_vendor = "apple", not(debug_assertions)))]
    let registry = {
        use tracing_oslog::OsLogger;
        let oslog_layer =
            OsLogger::new("cc.starryreverie.selector4nix", "default").with_filter(filter);
        registry.with(oslog_layer)
    };

    registry.init();
    Ok(())
}

pub async fn init_context(
    config: &Arc<AppConfiguration>,
    credentials: Arc<AppCredential>,
    cache_dir: Option<PathBuf>,
) -> AnyhowResult<Arc<AppContext>> {
    let has_persistent_cache = cache_dir.is_some();

    let database = match cache_dir {
        Some(cache_dir) => {
            if !cache_dir.is_dir() {
                return Err(anyhow::anyhow!(
                    "could not use `{}` as a cache directory",
                    cache_dir.display(),
                ));
            }
            let database_path = cache_dir.join(Path::new("main.redb"));
            let database = Database::builder().create(database_path)?;
            Arc::new(database)
        }
        None => {
            let database = Database::builder().create_with_backend(InMemoryBackend::new())?;
            Arc::new(database)
        }
    };

    let http_client = http_client_builder_factory(config)
        .build()
        .context("could not build HTTP client")?;

    let throttling_options = {
        // Per-substituter concurrency limits are keyed by the host NAR
        // requests actually go to, i.e. the storage host, so a storage
        // host shared by multiple substituters gets the last configured
        // limit.
        let per_host_max_concurrent_requests = config
            .substituters
            .iter()
            .filter_map(|sub_config| {
                sub_config.max_concurrent_requests.map(|limit| {
                    let host = sub_config
                        .storage_url
                        .as_ref()
                        .map_or(sub_config.url.host(), |storage_url| storage_url.host());
                    (host.to_string(), limit)
                })
            })
            .collect();
        ThrottlingOptions {
            default_max_concurrent_requests: config.network.max_concurrent_requests,
            per_host_max_concurrent_requests,
        }
    };

    // SNI proxy optimization auto-detects the CDN platform of every unique
    // substituter host, then assembles one `EndpointManager` per recognized
    // host with that platform's strictly separate SNI proxy list.
    let (streaming_http_client, endpoint_managers) = if config.fastly_optimization.enabled
        || config.cloudflare_optimization.enabled
    {
        // The main streaming client and all endpoint-bound clients share one
        // throttler so that the per-host concurrency limit of the logical
        // host is enforced across endpoints.
        let throttler = Arc::new(PerHostHttpThrottler::new(throttling_options));
        let streaming_http_client = Arc::new(StreamingClient::with_shared_throttler(
            http_client_builder_factory(config),
            Arc::clone(&throttler),
            config.network.chunked_streaming,
            config.network.streaming_chunk_max_len,
            config.network.streaming_window_max_len,
        ));

        let factory_config = Arc::clone(config);
        let factory: Arc<dyn Fn() -> ClientBuilder + Send + Sync> =
            Arc::new(move || http_client_builder_factory(&factory_config));
        let doh = Arc::new(DohResolver::new());
        let detector = Arc::new(CdnDetector::new(Arc::clone(&doh), http_client.clone()));
        let sni_proxy_sources = Arc::new(SniProxySourceProvider::new());
        let mut managers: Vec<Arc<EndpointManager>> = Vec::new();

        let mut seen_hosts = std::collections::HashSet::new();
        let mut detection_tasks = tokio::task::JoinSet::new();
        for sub_config in &config.substituters {
            let host = sub_config.url.host().to_string();
            if !seen_hosts.insert(host.clone()) {
                continue;
            }
            let base_url = sub_config.url.clone();
            let detector = Arc::clone(&detector);
            detection_tasks.spawn(async move {
                let detection = detector.detect(&base_url).await;
                (host, base_url, detection)
            });
        }

        while let Some(result) = detection_tasks.join_next().await {
            let (host, base_url, detection) = match result {
                Ok(result) => result,
                Err(error) => {
                    tracing::warn!(%error, "CDN detection task failed");
                    continue;
                }
            };
            let Some(detection) = detection else {
                tracing::info!(
                    %host,
                    "CDN platform not detected; substituter will use the system-DNS path"
                );
                continue;
            };

            tracing::info!(
                %host,
                platform = ?detection.kind,
                evidence = %detection.evidence,
                "detected substituter CDN platform"
            );

            let (user_candidates, platform_sni_proxy_sources, bandwidth_probe) = match detection
                .kind
            {
                EndpointOptimizationKind::Fastly if config.fastly_optimization.enabled => (
                    config.fastly_optimization.candidates.clone(),
                    config.fastly_optimization.sni_proxy_sources.clone(),
                    config.fastly_optimization.bandwidth_probe.clone(),
                ),
                EndpointOptimizationKind::Cloudflare if config.cloudflare_optimization.enabled => (
                    config.cloudflare_optimization.candidates.clone(),
                    config.cloudflare_optimization.sni_proxy_sources.clone(),
                    config.cloudflare_optimization.bandwidth_probe.clone(),
                ),
                kind => {
                    tracing::info!(
                        %host,
                        platform = ?kind,
                        "detected CDN platform is disabled; substituter will use the system-DNS path"
                    );
                    continue;
                }
            };

            let port = base_url.inner().port_or_known_default().unwrap_or(443);
            let pool = Arc::new(EndpointClientPool::new(
                host.clone(),
                port,
                Arc::clone(&factory),
                Arc::clone(&throttler),
                config.network.chunked_streaming,
                config.network.streaming_chunk_max_len,
                config.network.streaming_window_max_len,
                ENDPOINT_CLIENT_POOL_CAPACITY,
            ));
            let probing = Arc::new(EndpointProbingProvider::new(
                Arc::clone(&pool),
                config.network.nar_info_timeout,
            ));
            let user_candidate_count = user_candidates.len();
            let sni_proxy_source_count = platform_sni_proxy_sources.len();
            let active_bandwidth_probe = bandwidth_probe.enabled;
            let manager = Arc::new(EndpointManager::new(
                host.clone(),
                base_url,
                pool,
                probing,
                user_candidates,
                platform_sni_proxy_sources,
                Arc::clone(&sni_proxy_sources),
                Some(bandwidth_probe),
            ));

            tracing::info!(
                %host,
                platform = ?detection.kind,
                user_candidates = user_candidate_count,
                sni_proxy_sources = sni_proxy_source_count,
                active_bandwidth_probe,
                "SNI proxy optimization enabled for auto-detected substituter"
            );
            managers.push(manager);
        }

        let endpoint_managers = EndpointManagerRegistry::new(managers);

        // Refresh endpoints immediately, then periodically with a small
        // jitter. `refresh` handles its own errors; the loop must never
        // terminate or propagate a panic to the main process.
        tokio::spawn({
            let endpoint_managers = endpoint_managers.clone();
            async move {
                loop {
                    for manager in endpoint_managers.managers() {
                        manager.refresh().await;
                    }
                    let jitter = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|since_epoch| {
                            Duration::from_nanos(
                                u64::from(since_epoch.subsec_nanos())
                                    % ENDPOINT_REFRESH_MAX_JITTER.as_nanos() as u64,
                            )
                        })
                        .unwrap_or_default();
                    tokio::time::sleep(ENDPOINT_REFRESH_INTERVAL + jitter).await;
                }
            }
        });

        (streaming_http_client, endpoint_managers)
    } else {
        (
            Arc::new(StreamingClient::new(
                http_client_builder_factory(config),
                throttling_options,
                config.network.chunked_streaming,
                config.network.streaming_chunk_max_len,
                config.network.streaming_window_max_len,
            )),
            EndpointManagerRegistry::default(),
        )
    };

    let derivation_log_provider = Arc::new(ReqwestDerivationLogProvider::new(
        http_client.clone(),
        credentials.clone(),
    ));

    let substituter_probing_provider = Arc::new(ReqwestSubstituterProbingProvider::new(
        http_client.clone(),
        config.network.nar_info_timeout,
        credentials.clone(),
        endpoint_managers.clone(),
    ));

    let nar_info_provider = Arc::new(ReqwestNarInfoProvider::new(
        http_client.clone(),
        config.network.nar_info_timeout,
        credentials.clone(),
        endpoint_managers.clone(),
    ));

    let nar_directory_provider = Arc::new(ReqwestNarDirectoryProvider::new(
        http_client.clone(),
        credentials.clone(),
        endpoint_managers.clone(),
    ));

    let nar_stream_provider = Arc::new(ReqwestNarStreamProvider::new(
        streaming_http_client,
        credentials.clone(),
        endpoint_managers.clone(),
    ));

    let nar_transfer_metric = Arc::new(NarTransferMetric::new());

    let substituters = config
        .substituters
        .iter()
        .map(|sub_config| {
            let meta = SubstituterMeta::new(sub_config.url.clone(), sub_config.priority)
                .with_nar_info_timeout(sub_config.nar_info_timeout)
                .with_nar_timeout(sub_config.nar_timeout);
            let meta = match sub_config.storage_url.clone() {
                Some(storage_url) => meta.with_storage_url(storage_url),
                None => meta,
            };
            Substituter::new(meta, Availability::Normal)
        })
        .collect::<Vec<_>>();

    let substituter_repository = Arc::new({
        let substituter_repository = InMemorySubstituterRepository::new();
        for sub in &substituters {
            substituter_repository.save(sub.clone()).await;
        }
        substituter_repository
    });

    let nar_info_repository = {
        let cache_kv = Arc::new(CacheKv::new(database.clone(), "nar_info".into()));
        cache_kv.spawn_cleanup_task();
        Arc::new(CacheKvNarInfoRepository::new(cache_kv))
    };

    let nar_file_repository = {
        let cache_kv = Arc::new(CacheKv::new(database, "nar_file".into()));
        cache_kv.spawn_cleanup_task();
        Arc::new(CacheKvNarFileRepository::new(cache_kv))
    };

    let substituter_service = Arc::new(SubstituterService::new(config.network.periodic_probing));

    let nar_info_service = {
        let resolution_policy: Arc<dyn NarInfoResolutionPolicy> =
            match config.proxy.resolution_policy {
                ResolutionPolicyOption::Preference => Arc::new(PreferencePolicy::new(
                    nar_info_provider.clone(),
                    Duration::from_millis(config.network.tolerance),
                    config.network.ignore_nar_info_error,
                )),
                ResolutionPolicyOption::Tier => Arc::new(TierPolicy::new(
                    nar_info_provider.clone(),
                    config.network.ignore_nar_info_error,
                )),
            };
        Arc::new(NarInfoService::new(
            resolution_policy,
            substituter_repository.clone(),
            config.proxy.rewrite_nar_url,
        ))
    };

    let nar_file_service = Arc::new(NarFileService::new(
        nar_stream_provider,
        substituter_repository.clone(),
        config.cache.nar_file_ttl,
    ));

    let substituter_registry = Arc::new({
        let registry = RegistryBuilder::new()
            .factory(AsyncFactory::new({
                let substituter_service = substituter_service.clone();
                let substituter_probing_provider = substituter_probing_provider.clone();
                let substituter_repository = substituter_repository.clone();
                move |url: &Url| {
                    let substituter_service = substituter_service.clone();
                    let substituter_probing_provider = substituter_probing_provider.clone();
                    let substituter_repository = substituter_repository.clone();
                    let url = url.clone();
                    async move {
                        let init = substituter_repository.get(&url).await;
                        SubstituterActor::new(
                            init,
                            substituter_service,
                            substituter_probing_provider,
                            substituter_repository,
                        )
                        .run()
                    }
                }
            }))
            .build();
        for sub in &substituters {
            let _ = registry.get(sub.url()).await;
        }
        registry
    });

    let nar_info_registry = Arc::new(
        RegistryBuilder::new()
            .capacity(CapacityOption::Lru(config.cache.nar_info_cache_capacity))
            .expiration(ExpirationOption::Ttl(config.cache.nar_info_ttl))
            .factory(AsyncFactory::new({
                let nar_info_service = nar_info_service.clone();
                let nar_info_repository = nar_info_repository.clone();
                let nar_info_ttl = config.cache.nar_info_ttl;
                move |hash: &StorePathHash| {
                    let addr = NarInfoActor::new(
                        hash.clone(),
                        nar_info_service.clone(),
                        nar_info_repository.clone(),
                        nar_info_ttl,
                    )
                    .run();
                    async move { addr }
                }
            }))
            .build(),
    );

    let nar_file_registry = Arc::new(
        RegistryBuilder::new()
            .capacity(CapacityOption::Lru(config.cache.nar_file_cache_capacity))
            .expiration(ExpirationOption::Ttl(config.cache.nar_file_ttl))
            .factory(AsyncFactory::new({
                let nar_file_servicee = nar_file_service.clone();
                let nar_file_repository = nar_file_repository.clone();
                let nar_file_ttl = config.cache.nar_file_ttl;
                move |key: &NarFileKey| {
                    let addr = NarFileActor::new(
                        key.clone(),
                        nar_file_servicee.clone(),
                        nar_file_repository.clone(),
                        nar_file_ttl,
                    )
                    .run();
                    async move { addr }
                }
            }))
            .build(),
    );

    let get_derivation_log_usecase = GetDerivationLogUseCase::new(
        substituter_registry.clone(),
        substituter_repository.clone(),
        derivation_log_provider,
    );

    let resolve_nar_info_usecase = ResolveNarInfoUseCase::new(
        nar_info_registry.clone(),
        substituter_registry.clone(),
        nar_file_registry.clone(),
    );

    let list_nar_inner_directory_usecase = ListNarInnerDirectoryUseCase::new(
        substituter_registry.clone(),
        substituter_repository.clone(),
        nar_directory_provider,
    );

    let stream_nar_file_usecase = StreamNarFileUseCase::new(
        substituter_registry.clone(),
        nar_file_registry.clone(),
        nar_info_repository.clone(),
        nar_transfer_metric.clone(),
    );

    let add_substituter_usecase =
        AddSubstituterUseCase::new(substituter_repository.clone(), substituter_registry.clone());
    let enable_substituter_usecase =
        EnableSubstituterUseCase::new(substituter_repository.clone(), substituter_registry.clone());
    let disable_substituter_usecase = DisableSubstituterUseCase::new(
        substituter_repository.clone(),
        substituter_registry.clone(),
    );
    let add_sni_proxy_usecase =
        AddSniProxyUseCase::new(substituter_repository.clone(), endpoint_managers.clone());

    let get_dashboard_overview_usecase = GetDashboardOverviewUseCase::new(
        substituter_repository,
        nar_info_registry.clone(),
        nar_transfer_metric.clone(),
        credentials,
        endpoint_managers,
        config.cache.nar_info_cache_capacity,
        has_persistent_cache,
    );

    let get_dashboard_transferring_usecase =
        GetDashboardTransferringUseCase::new(nar_transfer_metric);

    let get_dashboard_cache_stats_usecase = GetDashboardCacheStatsUseCase::new(
        nar_info_registry,
        nar_file_registry,
        nar_info_repository,
        nar_file_repository,
        config.cache.clone(),
        has_persistent_cache,
    );

    let get_dashboard_config_summary_usecase =
        GetDashboardConfigSummaryUseCase::new(Arc::clone(config));

    Ok(Arc::new(AppContext {
        get_derivation_log_usecase,
        resolve_nar_info_usecase,
        list_nar_inner_directory_usecase,
        stream_nar_file_usecase,
        get_dashboard_overview_usecase,
        get_dashboard_transferring_usecase,
        get_dashboard_cache_stats_usecase,
        get_dashboard_config_summary_usecase,
        add_substituter_usecase,
        enable_substituter_usecase,
        disable_substituter_usecase,
        add_sni_proxy_usecase,
        cache_info: config.cache_info.clone(),
    }))
}

fn http_client_builder_factory(config: &AppConfiguration) -> ClientBuilder {
    Client::builder()
        .user_agent(SELF_USER_AGENT.as_str())
        .connect_timeout(config.network.nar_timeout)
}
