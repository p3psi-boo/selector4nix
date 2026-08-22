//! Endpoint discovery, admission probing, failure tracking and selection.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dashmap::DashMap;
use futures::StreamExt;
use tokio::time::Instant;

use crate::domain::common::url::Url;
use crate::domain::substituter::model::{
    CandidateSource, EndpointFailureKind, EndpointSnapshot, EndpointSnapshotStatus, EndpointState,
    SubstituterEndpoint, order_for_selection,
};
use crate::infrastructure::dns::doh_resolver::DohResolver;
use crate::infrastructure::fastly::region_derivation::derive_region_candidates;
use crate::infrastructure::provider::{
    EndpointClientPool, EndpointClientSet, EndpointProbingProvider, ProbeEndpointError,
};

/// Maximum number of endpoints admission-probed concurrently during refresh.
const PROBE_CONCURRENCY: usize = 8;

/// Merge candidate IPs from all discovery sources, deduplicated; the first
/// source (DoH, then user-configured, then derived regions) wins.
fn collect_candidates(
    discovered: &[Ipv4Addr],
    user_candidates: &[IpAddr],
    derive_regions: bool,
) -> Vec<(IpAddr, CandidateSource)> {
    fn push(
        ip: IpAddr,
        source: CandidateSource,
        candidates: &mut Vec<(IpAddr, CandidateSource)>,
        seen: &mut BTreeSet<IpAddr>,
    ) {
        if seen.insert(ip) {
            candidates.push((ip, source));
        }
    }
    let mut candidates: Vec<(IpAddr, CandidateSource)> = Vec::new();
    let mut seen: BTreeSet<IpAddr> = BTreeSet::new();
    for ip in discovered {
        push(
            IpAddr::V4(*ip),
            CandidateSource::DnsDoh,
            &mut candidates,
            &mut seen,
        );
    }
    for ip in user_candidates {
        push(
            *ip,
            CandidateSource::UserConfigured,
            &mut candidates,
            &mut seen,
        );
    }
    if derive_regions {
        let seed: Vec<IpAddr> = candidates.iter().map(|(ip, _)| *ip).collect();
        for ip in seed {
            if let IpAddr::V4(v4) = ip {
                for derived in derive_region_candidates(&v4) {
                    push(
                        IpAddr::V4(derived),
                        CandidateSource::DerivedRegion,
                        &mut candidates,
                        &mut seen,
                    );
                }
            }
        }
    }
    candidates
}

/// Runtime state of endpoint candidates for a single logical host:
/// discovery, admission probing, failure tracking and selection ordering.
pub struct EndpointManager {
    endpoints: DashMap<IpAddr, SubstituterEndpoint>,
    pool: Arc<EndpointClientPool>,
    probing: Arc<EndpointProbingProvider>,
    doh: Arc<DohResolver>,
    host: String,
    base_url: Url,
    user_candidates: Vec<IpAddr>,
    derive_regions: bool,
    /// First endpoint of the previous selection order, for change logging.
    last_selected: Mutex<Option<IpAddr>>,
}

impl EndpointManager {
    pub fn new(
        host: String,
        base_url: Url,
        pool: Arc<EndpointClientPool>,
        probing: Arc<EndpointProbingProvider>,
        doh: Arc<DohResolver>,
        user_candidates: Vec<IpAddr>,
        derive_regions: bool,
    ) -> Self {
        Self {
            endpoints: DashMap::new(),
            pool,
            probing,
            doh,
            host,
            base_url,
            user_candidates,
            derive_regions,
            last_selected: Mutex::new(None),
        }
    }

    /// Discover candidates (DoH ∪ configured ∪ derived), keep existing
    /// endpoint states untouched, and admission-probe all pending endpoints.
    pub async fn refresh(&self) {
        let discovered = self.doh.query_a(&self.host).await;
        if discovered.is_empty() && self.user_candidates.is_empty() {
            tracing::warn!(
                host = %self.host,
                "endpoint discovery yielded no candidates; keeping existing endpoints"
            );
        }
        let candidates =
            collect_candidates(&discovered, &self.user_candidates, self.derive_regions);

        for (ip, source) in &candidates {
            self.endpoints
                .entry(*ip)
                .or_insert_with(|| SubstituterEndpoint::new(*ip, *source));
        }

        // Half-open retry: cooling endpoints whose period expired become
        // pending again and are re-probed below.
        let now = Instant::now();
        for mut entry in self.endpoints.iter_mut() {
            if matches!(entry.state(), EndpointState::Cooling { until } if now >= until) {
                *entry.value_mut() = entry.value().with_state(EndpointState::Pending);
            }
        }

        let pending: Vec<SubstituterEndpoint> = self
            .endpoints
            .iter()
            .filter(|entry| entry.state() == EndpointState::Pending)
            .map(|entry| entry.value().clone())
            .collect();

        let results: Vec<(SubstituterEndpoint, Result<Duration, ProbeEndpointError>)> =
            futures::stream::iter(pending.into_iter().map(|endpoint| {
                let probing = Arc::clone(&self.probing);
                let base_url = self.base_url.clone();
                async move {
                    let result = probing.probe_endpoint(&base_url, endpoint.ip()).await;
                    (endpoint, result)
                }
            }))
            .buffer_unordered(PROBE_CONCURRENCY)
            .collect()
            .await;

        let now = Instant::now();
        let mut admitted = 0usize;
        for (endpoint, result) in results {
            let updated = match result {
                Ok(latency) => {
                    admitted += 1;
                    endpoint.on_admission_success(latency)
                }
                Err(ProbeEndpointError::Certificate { message, .. }) => {
                    tracing::debug!(ip = %endpoint.ip(), %message, "endpoint failed TLS admission");
                    endpoint.on_failure(EndpointFailureKind::Certificate, now)
                }
                Err(ProbeEndpointError::Transient { message, .. }) => {
                    tracing::debug!(ip = %endpoint.ip(), %message, "endpoint admission probe failed");
                    endpoint.on_failure(EndpointFailureKind::Transient, now)
                }
            };
            self.endpoints.insert(endpoint.ip(), updated);
        }

        self.update_selected();

        let usable: Vec<String> = self
            .ordered_usable()
            .into_iter()
            .map(|ip| {
                let latency = match self.endpoints.get(&ip).map(|e| e.state()) {
                    Some(EndpointState::Usable { admission_latency }) => {
                        format!("{admission_latency:?}")
                    }
                    _ => "?".to_string(),
                };
                format!("{ip} ({latency})")
            })
            .collect();
        tracing::info!(
            host = %self.host,
            candidates = candidates.len(),
            admitted,
            ?usable,
            "endpoint refresh completed"
        );
    }

    /// Currently usable endpoints ordered by admission latency, ascending.
    /// Empty when none are usable; callers then fall back to the default path.
    pub fn ordered_usable(&self) -> Vec<IpAddr> {
        let endpoints: Vec<SubstituterEndpoint> = self
            .endpoints
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        order_for_selection(&endpoints, Instant::now())
            .into_iter()
            .map(|endpoint| endpoint.ip())
            .collect()
    }

    /// Report a usage failure: transient failures cool the endpoint down,
    /// certificate failures mark it permanently incompatible.
    pub fn report_failure(&self, ip: IpAddr, kind: EndpointFailureKind) {
        if let Some(mut entry) = self.endpoints.get_mut(&ip) {
            *entry.value_mut() = entry.value().on_failure(kind, Instant::now());
            tracing::info!(
                host = %self.host,
                %ip,
                ?kind,
                state = ?entry.value().state(),
                "endpoint state changed after reported failure"
            );
        }
    }

    /// All known endpoints, usable ones first ordered by admission latency,
    /// then pending, then cooling, then incompatible.
    pub fn snapshot(&self) -> Vec<EndpointSnapshot> {
        fn rank(status: &EndpointSnapshotStatus) -> (u8, Duration) {
            match status {
                EndpointSnapshotStatus::Usable { admission_latency } => (0, *admission_latency),
                EndpointSnapshotStatus::Pending => (1, Duration::ZERO),
                EndpointSnapshotStatus::Cooling => (2, Duration::ZERO),
                EndpointSnapshotStatus::Incompatible => (3, Duration::ZERO),
            }
        }
        let mut snapshots: Vec<EndpointSnapshot> = self
            .endpoints
            .iter()
            .map(|entry| EndpointSnapshot::of(entry.value()))
            .collect();
        snapshots.sort_by(|a, b| rank(&a.status).cmp(&rank(&b.status)).then(a.ip.cmp(&b.ip)));
        snapshots
    }

    /// Log when the first-choice endpoint differs from the previous refresh.
    fn update_selected(&self) {
        let selected = self.ordered_usable().first().copied();
        let mut last = self
            .last_selected
            .lock()
            .expect("last_selected mutex is not poisoned");
        if *last == selected {
            return;
        }
        if let (Some(previous), Some(current)) = (*last, selected) {
            let latency_of = |ip: IpAddr| match self.endpoints.get(&ip).map(|e| e.state()) {
                Some(EndpointState::Usable { admission_latency }) => {
                    format!("{admission_latency:?}")
                }
                _ => "?".to_string(),
            };
            tracing::info!(
                host = %self.host,
                from = %previous,
                from_latency = %latency_of(previous),
                to = %current,
                to_latency = %latency_of(current),
                "selected endpoint changed"
            );
        }
        *last = selected;
    }

    /// Endpoint-bound clients for `ip`; `None` when `ip` is not a known endpoint.
    pub fn client_for(&self, ip: IpAddr) -> Option<EndpointClientSet> {
        if self.endpoints.contains_key(&ip) {
            Some(self.pool.get_or_build(ip))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use reqwest::Client;
    use selector4nix_streaming::throttler::{PerHostHttpThrottler, ThrottlingOptions};

    use super::*;
    use crate::infrastructure::provider::EndpointProbingProvider;

    fn ip(octet: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(151, 101, 1, octet))
    }

    fn make_manager(user_candidates: Vec<IpAddr>, derive_regions: bool) -> EndpointManager {
        let pool = Arc::new(EndpointClientPool::new(
            "cache.nixos.org".to_string(),
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
            Duration::from_secs(5),
        ));
        EndpointManager::new(
            "cache.nixos.org".to_string(),
            Url::new("https://cache.nixos.org").unwrap(),
            pool,
            probing,
            Arc::new(DohResolver::new()),
            user_candidates,
            derive_regions,
        )
    }

    #[test]
    fn candidates_are_merged_deduplicated_and_sourced() {
        let discovered = vec![Ipv4Addr::new(151, 101, 1, 91)];
        let user = vec![IpAddr::V4(Ipv4Addr::new(151, 101, 1, 91)), ip(1)];

        let candidates = collect_candidates(&discovered, &user, false);

        assert_eq!(
            candidates,
            vec![
                (
                    IpAddr::V4(Ipv4Addr::new(151, 101, 1, 91)),
                    CandidateSource::DnsDoh
                ),
                (ip(1), CandidateSource::UserConfigured),
            ]
        );
    }

    #[test]
    fn region_derivation_appends_derived_candidates_without_duplicates() {
        let discovered = vec![Ipv4Addr::new(151, 101, 1, 91)];

        let candidates = collect_candidates(&discovered, &[], true);

        assert_eq!(candidates[0].1, CandidateSource::DnsDoh);
        assert!(candidates.len() > 1);
        assert!(
            candidates[1..]
                .iter()
                .all(|(_, source)| *source == CandidateSource::DerivedRegion)
        );
        let ips: Vec<_> = candidates.iter().map(|(ip, _)| *ip).collect();
        let mut deduped = ips.clone();
        deduped.dedup();
        deduped.sort();
        let mut sorted = ips.clone();
        sorted.sort();
        assert_eq!(sorted, deduped);
    }

    #[test]
    fn existing_endpoint_states_are_not_reset_by_candidates() {
        let manager = make_manager(vec![ip(1), ip(2)], false);

        let now = Instant::now();
        manager.endpoints.insert(
            ip(1),
            SubstituterEndpoint::new(ip(1), CandidateSource::UserConfigured)
                .on_admission_success(Duration::from_millis(50)),
        );

        // Re-discovering ip(1) must not reset its Usable state; ip(2) is new.
        for (candidate, source) in collect_candidates(&[], &manager.user_candidates, false) {
            manager
                .endpoints
                .entry(candidate)
                .or_insert_with(|| SubstituterEndpoint::new(candidate, source));
        }

        assert!(matches!(
            manager.endpoints.get(&ip(1)).unwrap().state(),
            EndpointState::Usable { .. }
        ));
        assert_eq!(
            manager.endpoints.get(&ip(2)).unwrap().state(),
            EndpointState::Pending
        );
        let _ = now;
    }

    #[test]
    fn report_failure_transitions_state() {
        let manager = make_manager(vec![], false);
        manager.endpoints.insert(
            ip(1),
            SubstituterEndpoint::new(ip(1), CandidateSource::DnsDoh)
                .on_admission_success(Duration::from_millis(50)),
        );
        manager.endpoints.insert(
            ip(2),
            SubstituterEndpoint::new(ip(2), CandidateSource::DnsDoh)
                .on_admission_success(Duration::from_millis(60)),
        );

        manager.report_failure(ip(1), EndpointFailureKind::Transient);
        manager.report_failure(ip(2), EndpointFailureKind::Certificate);
        // Unknown IPs are ignored.
        manager.report_failure(ip(3), EndpointFailureKind::Transient);

        assert!(matches!(
            manager.endpoints.get(&ip(1)).unwrap().state(),
            EndpointState::Cooling { .. }
        ));
        assert_eq!(
            manager.endpoints.get(&ip(2)).unwrap().state(),
            EndpointState::Incompatible
        );
        assert!(manager.ordered_usable().is_empty());
    }

    #[test]
    fn client_for_returns_clients_only_for_known_endpoints() {
        let manager = make_manager(vec![], false);
        manager.endpoints.insert(
            ip(1),
            SubstituterEndpoint::new(ip(1), CandidateSource::DnsDoh),
        );

        assert!(manager.client_for(ip(1)).is_some());
        assert!(manager.client_for(ip(2)).is_none());
    }

    #[test]
    fn snapshot_orders_usable_by_latency_then_pending_cooling_incompatible() {
        let manager = make_manager(vec![], false);
        let now = Instant::now();
        let insert = |octet: u8, state: EndpointState| {
            manager.endpoints.insert(
                ip(octet),
                SubstituterEndpoint::new(ip(octet), CandidateSource::DnsDoh).with_state(state),
            );
        };
        insert(4, EndpointState::Incompatible);
        insert(
            3,
            EndpointState::Cooling {
                until: now + Duration::from_secs(60),
            },
        );
        insert(5, EndpointState::Pending);
        insert(
            1,
            EndpointState::Usable {
                admission_latency: Duration::from_millis(200),
            },
        );
        insert(
            2,
            EndpointState::Usable {
                admission_latency: Duration::from_millis(50),
            },
        );

        let snapshot = manager.snapshot();

        let ips: Vec<IpAddr> = snapshot.iter().map(|s| s.ip).collect();
        assert_eq!(ips, vec![ip(2), ip(1), ip(5), ip(3), ip(4)]);
        assert_eq!(
            snapshot[0].status,
            EndpointSnapshotStatus::Usable {
                admission_latency: Duration::from_millis(50)
            }
        );
        assert_eq!(snapshot[2].status, EndpointSnapshotStatus::Pending);
        assert_eq!(snapshot[3].status, EndpointSnapshotStatus::Cooling);
        assert_eq!(snapshot[4].status, EndpointSnapshotStatus::Incompatible);
    }

    #[test]
    fn update_selected_tracks_the_first_choice_endpoint() {
        let manager = make_manager(vec![], false);

        // First round: nothing logged, but the selection is recorded.
        manager.update_selected();
        assert_eq!(*manager.last_selected.lock().unwrap(), None);

        manager.endpoints.insert(
            ip(1),
            SubstituterEndpoint::new(ip(1), CandidateSource::DnsDoh)
                .on_admission_success(Duration::from_millis(100)),
        );
        manager.update_selected();
        assert_eq!(*manager.last_selected.lock().unwrap(), Some(ip(1)));

        // A failure cools ip(1) down; no usable endpoint remains.
        manager.report_failure(ip(1), EndpointFailureKind::Transient);
        manager.update_selected();
        assert_eq!(*manager.last_selected.lock().unwrap(), None);
    }
}
