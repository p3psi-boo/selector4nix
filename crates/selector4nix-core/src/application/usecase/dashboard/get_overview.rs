use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;

use serde::Serialize;

use crate::application::actor::nar_info::NarInfoActorRegistry;
use crate::domain::common::url::Url;
use crate::domain::substituter::SubstituterRepository;
use crate::domain::substituter::model::{
    Availability, CandidateSource, EndpointSnapshotStatus, Priority,
};
use crate::infrastructure::config::{AppConfiguration, AppCredential};
use crate::infrastructure::endpoint::registry::EndpointManagerRegistry;
use crate::infrastructure::metric::NarTransferMetric;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct OverviewData {
    summary: OverviewSummaryData,
    substituters: Vec<OverviewSubstituterItemData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct OverviewSummaryData {
    available_substituters: usize,
    total_substituters: usize,
    transferring_nar_files: usize,
    nar_info_cache_size: usize,
    nar_info_cache_capacity: NonZeroUsize,
    cache_mode: CacheMode,
    bytes_per_second: u64,
    recent_failures: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum CacheMode {
    Persistent,
    InMemory,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct OverviewSubstituterItemData {
    url: Url,
    storage_url: Url,
    priority: Priority,
    has_credential: bool,
    status: SubstituterStatus,
    endpoints: Vec<OverviewEndpointItemData>,
    supports_sni: bool,
    runtime_changed: bool,
    status_detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct OverviewEndpointItemData {
    ip: IpAddr,
    source: String,
    status: String,
    detail: String,
    admission_ms: Option<u128>,
    ttfb_ms: Option<u128>,
    bytes_per_second: Option<u64>,
    runtime_added: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum SubstituterStatus {
    Normal,
    Offline,
    ServiceError,
    MaybeReady,
    Disabled,
}

pub struct GetDashboardOverviewUseCase {
    config: Arc<AppConfiguration>,
    substituter_repository: Arc<dyn SubstituterRepository>,
    nar_info_registry: Arc<NarInfoActorRegistry>,
    nar_transfer_metric: Arc<NarTransferMetric>,
    credentials: Arc<AppCredential>,
    endpoint_managers: EndpointManagerRegistry,
    nar_info_cache_capacity: NonZeroUsize,
    cache_mode: CacheMode,
}

impl GetDashboardOverviewUseCase {
    pub fn new(
        substituter_repository: Arc<dyn SubstituterRepository>,
        nar_info_registry: Arc<NarInfoActorRegistry>,
        nar_transfer_metric: Arc<NarTransferMetric>,
        credentials: Arc<AppCredential>,
        endpoint_managers: EndpointManagerRegistry,
        nar_info_cache_capacity: NonZeroUsize,
        has_persistent_cache: bool,
        config: Arc<AppConfiguration>,
    ) -> Self {
        Self {
            config,
            substituter_repository,
            nar_info_registry,
            nar_transfer_metric,
            credentials,
            endpoint_managers,
            nar_info_cache_capacity,
            cache_mode: if has_persistent_cache {
                CacheMode::Persistent
            } else {
                CacheMode::InMemory
            },
        }
    }

    pub async fn run(&self) -> OverviewData {
        let substituters = self.substituter_repository.query_all().await;

        let summary = OverviewSummaryData {
            bytes_per_second: self
                .nar_transfer_metric
                .transferring()
                .iter()
                .map(|e| e.bytes_per_second())
                .sum(),
            recent_failures: self
                .nar_transfer_metric
                .recent()
                .iter()
                .filter(|e| e.outcome.is_some_and(|s| s.starts_with("Failed")))
                .count(),
            available_substituters: substituters.iter().filter(|s| s.is_selectable()).count(),
            total_substituters: substituters.len(),
            transferring_nar_files: self.nar_transfer_metric.transferring_count(),
            nar_info_cache_size: self.nar_info_registry.entry_count().await,
            nar_info_cache_capacity: self.nar_info_cache_capacity,
            cache_mode: self.cache_mode,
        };

        let mut substituters = substituters
            .iter()
            .map(|s| OverviewSubstituterItemData {
                url: s.url().clone(),
                storage_url: s.target().storage_url().clone(),
                priority: s.priority(),
                has_credential: self.credentials.lookup(s.url()).is_some(),
                status: if !s.is_enabled() {
                    SubstituterStatus::Disabled
                } else {
                    match s.availability() {
                        Availability::Normal => SubstituterStatus::Normal,
                        Availability::Offline { .. } => SubstituterStatus::Offline,
                        Availability::ServiceError { .. } => SubstituterStatus::ServiceError,
                        Availability::MaybeReady { .. } => SubstituterStatus::MaybeReady,
                    }
                },
                endpoints: self.endpoints_for(s.url()),
                supports_sni: self.endpoint_managers.for_host(s.url().host()).is_some(),
                runtime_changed: !s.is_enabled() || !self.config.substituters.iter().any(|c| &c.url == s.url()) || self.endpoint_managers.for_host(s.url().host()).is_some_and(|m| !m.runtime_candidates().is_empty()),
                status_detail: if !s.is_enabled() {
                    "Excluded from selection. Enable to use this upstream again. Restarts restore configured upstreams.".into()
                } else {
                    match s.availability() {
                        Availability::Normal => "Available for cache queries and downloads.".into(),
                        Availability::MaybeReady { .. } => "Eligible for a recovery attempt; a successful request confirms recovery.".into(),
                        state @ (Availability::Offline { detected_at } | Availability::ServiceError { detected_at, .. }) => {
                            let remaining = state.retry_duration().unwrap_or_default().saturating_sub(detected_at.elapsed()).as_secs();
                            format!("Temporarily excluded after an upstream failure. Recovery eligible in {remaining}s. If this persists, check the upstream URL and service logs.")
                        }
                    }
                },
            })
            .collect::<Vec<_>>();
        substituters.sort_by(|lhs, rhs| (lhs.priority, &lhs.url).cmp(&(rhs.priority, &rhs.url)));

        OverviewData {
            summary,
            substituters,
        }
    }

    /// Endpoint snapshot for the substituter, non-empty only when a manager
    /// is registered for its host.
    fn endpoints_for(&self, url: &Url) -> Vec<OverviewEndpointItemData> {
        let Some(manager) = self.endpoint_managers.for_host(url.host()) else {
            return Vec::new();
        };
        let runtime_candidates = manager.runtime_candidates();
        manager
            .snapshot()
            .into_iter()
            .map(|snapshot| {
                let (status, detail, admission_ms, ttfb_ms, bytes_per_second) = match snapshot
                    .status
                {
                    EndpointSnapshotStatus::Usable {
                        admission_latency,
                        bandwidth,
                    } => (
                        "Usable",
                        "Available for downloads.".to_string(),
                        Some(admission_latency.as_millis()),
                        bandwidth.map(|b| b.time_to_first_byte.as_millis()),
                        bandwidth.map(|b| b.bytes_per_second),
                    ),
                    EndpointSnapshotStatus::Pending => (
                        "Pending",
                        "Waiting for an admission probe. Refreshes automatically.".into(),
                        None,
                        None,
                        None,
                    ),
                    EndpointSnapshotStatus::Cooling => (
                        "Cooling",
                        format!(
                            "Temporarily avoided after a network failure. Eligible again in {}s.",
                            snapshot.retry_after_secs.unwrap_or(0)
                        ),
                        None,
                        None,
                        None,
                    ),
                    EndpointSnapshotStatus::Incompatible => (
                        "Incompatible",
                        "TLS certificate does not match this upstream. Use a different proxy IP."
                            .into(),
                        None,
                        None,
                        None,
                    ),
                };
                OverviewEndpointItemData {
                    ip: snapshot.ip,
                    source: match snapshot.source {
                        CandidateSource::SniProxy => "SNI proxy",
                        CandidateSource::UserConfigured => "Configured",
                    }
                    .into(),
                    status: status.into(),
                    detail,
                    admission_ms,
                    ttfb_ms,
                    bytes_per_second,
                    runtime_added: runtime_candidates.contains(&snapshot.ip),
                }
            })
            .collect()
    }
}
