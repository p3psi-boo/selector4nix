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
use crate::infrastructure::config::AppCredential;
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
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct OverviewEndpointItemData {
    ip: IpAddr,
    source: String,
    status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum SubstituterStatus {
    Normal,
    Offline,
    ServiceError,
    MaybeReady,
}

pub struct GetDashboardOverviewUseCase {
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
    ) -> Self {
        Self {
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
            available_substituters: substituters.iter().filter(|s| !s.is_unavailable()).count(),
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
                status: match s.availability() {
                    Availability::Normal => SubstituterStatus::Normal,
                    Availability::Offline { .. } => SubstituterStatus::Offline,
                    Availability::ServiceError { .. } => SubstituterStatus::ServiceError,
                    Availability::MaybeReady { .. } => SubstituterStatus::MaybeReady,
                },
                endpoints: self.endpoints_for(s.url()),
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
        manager
            .snapshot()
            .into_iter()
            .map(|snapshot| OverviewEndpointItemData {
                ip: snapshot.ip,
                source: match snapshot.source {
                    CandidateSource::DnsDoh => "DoH",
                    CandidateSource::SniProxy => "SNI proxy",
                    CandidateSource::UserConfigured => "configured",
                    CandidateSource::DerivedRegion => "derived",
                }
                .to_string(),
                status: match snapshot.status {
                    EndpointSnapshotStatus::Usable {
                        admission_latency,
                        bandwidth,
                    } => {
                        if let Some(bandwidth) = bandwidth {
                            format!(
                                "Usable (admission {}ms, TTFB {}ms, {:.1} MiB/s)",
                                admission_latency.as_millis(),
                                bandwidth.time_to_first_byte.as_millis(),
                                bandwidth.bytes_per_second as f64 / (1024.0 * 1024.0),
                            )
                        } else {
                            format!("Usable (admission {}ms)", admission_latency.as_millis())
                        }
                    }
                    EndpointSnapshotStatus::Pending => "Pending".to_string(),
                    EndpointSnapshotStatus::Cooling => "Cooling".to_string(),
                    EndpointSnapshotStatus::Incompatible => "Incompatible".to_string(),
                },
            })
            .collect()
    }
}
