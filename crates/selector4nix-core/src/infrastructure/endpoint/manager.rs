//! SNI proxy discovery, admission probing, failure tracking and selection.

use std::collections::BTreeSet;
use std::net::IpAddr;
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
use crate::infrastructure::config::{BandwidthProbeConfiguration, SniProxySourceConfiguration};
use crate::infrastructure::provider::{
    EndpointClientPool, EndpointClientSet, EndpointProbingProvider, ProbeEndpointError,
    SniProxySourceProvider,
};

/// Maximum number of endpoints admission-probed concurrently during refresh.
const PROBE_CONCURRENCY: usize = 8;

/// Merge SNI proxy list IPs with extra configured IPs, deduplicated; list
/// entries win over user-configured literals for the same address.
fn collect_candidates(
    sni_proxies: &[IpAddr],
    user_candidates: &[IpAddr],
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
    for ip in sni_proxies {
        push(*ip, CandidateSource::SniProxy, &mut candidates, &mut seen);
    }
    for ip in user_candidates {
        push(
            *ip,
            CandidateSource::UserConfigured,
            &mut candidates,
            &mut seen,
        );
    }
    candidates
}

/// Runtime state of SNI proxy candidates for a single logical host:
/// list refresh, admission probing, failure tracking and selection ordering.
pub struct EndpointManager {
    endpoints: DashMap<IpAddr, SubstituterEndpoint>,
    pool: Arc<EndpointClientPool>,
    probing: Arc<EndpointProbingProvider>,
    host: String,
    base_url: Url,
    user_candidates: Vec<IpAddr>,
    /// Extra SNI proxy IPs added at runtime; not written back to config.
    runtime_candidates: Mutex<Vec<IpAddr>>,
    /// Platform-specific local or remote lists of SNI proxy IPs.
    sni_proxy_sources: Vec<SniProxySourceConfiguration>,
    sni_proxy_source_provider: Arc<SniProxySourceProvider>,
    /// Optional active bounded-download benchmark for this platform.
    bandwidth_probe: Option<BandwidthProbeConfiguration>,
    /// First endpoint of the previous selection order, for change logging.
    last_selected: Mutex<Option<IpAddr>>,
}

impl EndpointManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: String,
        base_url: Url,
        pool: Arc<EndpointClientPool>,
        probing: Arc<EndpointProbingProvider>,
        user_candidates: Vec<IpAddr>,
        sni_proxy_sources: Vec<SniProxySourceConfiguration>,
        sni_proxy_source_provider: Arc<SniProxySourceProvider>,
        bandwidth_probe: Option<BandwidthProbeConfiguration>,
    ) -> Self {
        Self {
            endpoints: DashMap::new(),
            pool,
            probing,
            host,
            base_url,
            user_candidates,
            runtime_candidates: Mutex::new(Vec::new()),
            sni_proxy_sources,
            sni_proxy_source_provider,
            bandwidth_probe,
            last_selected: Mutex::new(None),
        }
    }

    /// The logical host this manager is bound to.
    pub fn host(&self) -> &str {
        &self.host
    }

    fn extra_candidates(&self) -> Vec<IpAddr> {
        let mut extras = self.user_candidates.clone();
        let runtime = self
            .runtime_candidates
            .lock()
            .expect("runtime_candidates mutex is not poisoned");
        for ip in runtime.iter() {
            if !extras.contains(ip) {
                extras.push(*ip);
            }
        }
        extras
    }

    /// Register an extra SNI proxy IP for this host. Returns `false` when the
    /// address is already a candidate. The caller should then admission-probe.
    pub fn register_runtime_proxy(&self, ip: IpAddr) -> bool {
        if self.user_candidates.contains(&ip) || self.endpoints.contains_key(&ip) {
            return false;
        }
        let mut runtime = self
            .runtime_candidates
            .lock()
            .expect("runtime_candidates mutex is not poisoned");
        if runtime.contains(&ip) {
            return false;
        }
        runtime.push(ip);
        drop(runtime);
        self.endpoints
            .entry(ip)
            .or_insert_with(|| SubstituterEndpoint::new(ip, CandidateSource::UserConfigured));
        true
    }

    /// Register `ip` and admission-probe it immediately so it can be selected
    /// without waiting for the periodic refresh.
    pub async fn add_sni_proxy(&self, ip: IpAddr) -> bool {
        if !self.register_runtime_proxy(ip) {
            return false;
        }
        tracing::info!(host = %self.host, %ip, "added runtime SNI proxy");
        self.admit_one(ip).await;
        self.benchmark_endpoints().await;
        self.update_selected();
        true
    }

    async fn admit_one(&self, ip: IpAddr) {
        let Some(endpoint) = self.endpoints.get(&ip).map(|entry| entry.clone()) else {
            return;
        };
        if endpoint.state() != EndpointState::Pending {
            return;
        }
        let result = self.probing.probe_endpoint(&self.base_url, ip).await;
        let now = Instant::now();
        let updated = match result {
            Ok(latency) => endpoint.on_admission_success(latency),
            Err(ProbeEndpointError::Certificate { message, .. }) => {
                tracing::debug!(%ip, %message, "endpoint failed TLS admission");
                endpoint.on_failure(EndpointFailureKind::Certificate, now)
            }
            Err(ProbeEndpointError::Transient { message, .. }) => {
                tracing::debug!(%ip, %message, "endpoint admission probe failed");
                endpoint.on_failure(EndpointFailureKind::Transient, now)
            }
        };
        self.endpoints.insert(ip, updated);
    }

    /// Reload SNI proxy lists, keep existing endpoint states untouched, and
    /// admission-probe all pending endpoints.
    pub async fn refresh(&self) {
        let mut sni_proxies = Vec::new();
        for source in &self.sni_proxy_sources {
            sni_proxies.extend(self.sni_proxy_source_provider.endpoints(source).await);
        }
        let extras = self.extra_candidates();
        if sni_proxies.is_empty() && extras.is_empty() {
            tracing::warn!(
                host = %self.host,
                "SNI proxy discovery yielded no candidates; keeping existing endpoints"
            );
        }
        let candidates = collect_candidates(&sni_proxies, &extras);

        // SNI proxy lists are refreshable inventories. Drop relays that
        // disappeared from every current source so a local file edit or remote
        // list refresh takes effect without restarting selector4nix.
        let current_candidates: BTreeSet<IpAddr> = candidates.iter().map(|(ip, _)| *ip).collect();
        self.endpoints
            .retain(|ip, _| current_candidates.contains(ip));

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

        self.benchmark_endpoints().await;

        self.update_selected();

        let usable: Vec<String> = self
            .ordered_usable()
            .into_iter()
            .map(|ip| {
                let latency = match self.endpoints.get(&ip).map(|e| e.state()) {
                    Some(EndpointState::Usable {
                        admission_latency, ..
                    }) => {
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

    async fn benchmark_endpoints(&self) {
        let Some(config) = self
            .bandwidth_probe
            .as_ref()
            .filter(|config| config.enabled)
        else {
            return;
        };
        let now = Instant::now();
        let pending: Vec<SubstituterEndpoint> = self
            .endpoints
            .iter()
            .filter(|entry| entry.needs_bandwidth_probe(now, config.refresh_interval))
            .map(|entry| entry.value().clone())
            .collect();
        if pending.is_empty() {
            return;
        }

        let results = futures::stream::iter(pending.into_iter().map(|endpoint| {
            let probing = Arc::clone(&self.probing);
            let config = config.clone();
            async move {
                let result = probing.benchmark_endpoint(endpoint.ip(), &config).await;
                (endpoint.ip(), result)
            }
        }))
        .buffer_unordered(config.max_concurrent_probes.get())
        .collect::<Vec<_>>()
        .await;

        for (ip, result) in results {
            match result {
                Ok(measurement) => {
                    if let Some(mut endpoint) = self.endpoints.get_mut(&ip) {
                        *endpoint.value_mut() = endpoint.on_bandwidth_success(measurement);
                    }
                }
                Err(error) => {
                    tracing::debug!(%ip, %error, "endpoint bandwidth benchmark failed");
                }
            }
        }
    }

    /// Currently usable endpoints ordered by bandwidth-aware selection score.
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

    /// All known endpoints, usable ones first ordered by the active selection score,
    /// then pending, then cooling, then incompatible.
    pub fn snapshot(&self) -> Vec<EndpointSnapshot> {
        fn rank(status: &EndpointSnapshotStatus) -> (u8, Duration) {
            match status {
                EndpointSnapshotStatus::Usable {
                    bandwidth: Some(measurement),
                    ..
                } => (0, measurement.estimated_download_time(10 * 1024 * 1024)),
                EndpointSnapshotStatus::Usable {
                    admission_latency,
                    bandwidth: None,
                } => (1, *admission_latency),
                EndpointSnapshotStatus::Pending => (2, Duration::ZERO),
                EndpointSnapshotStatus::Cooling => (3, Duration::ZERO),
                EndpointSnapshotStatus::Incompatible => (4, Duration::ZERO),
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
                Some(EndpointState::Usable {
                    admission_latency, ..
                }) => {
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
    use std::net::Ipv4Addr;
    use std::num::NonZeroUsize;

    use reqwest::Client;
    use selector4nix_streaming::throttler::{PerHostHttpThrottler, ThrottlingOptions};

    use super::*;
    use crate::infrastructure::provider::{EndpointProbingProvider, SniProxySourceProvider};

    fn ip(octet: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(151, 101, 1, octet))
    }

    fn make_manager(user_candidates: Vec<IpAddr>) -> EndpointManager {
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
            user_candidates,
            Vec::new(),
            Arc::new(SniProxySourceProvider::new()),
            None,
        )
    }

    #[test]
    fn candidates_are_merged_deduplicated_and_sourced() {
        let proxies = vec![ip(91), ip(1)];
        let user = vec![ip(91), ip(2)];

        let candidates = collect_candidates(&proxies, &user);

        assert_eq!(
            candidates,
            vec![
                (ip(91), CandidateSource::SniProxy),
                (ip(1), CandidateSource::SniProxy),
                (ip(2), CandidateSource::UserConfigured),
            ]
        );
    }

    #[test]
    fn host_returns_the_bound_host() {
        let manager = make_manager(vec![]);
        assert_eq!(manager.host(), "cache.nixos.org");
    }

    #[test]
    fn register_runtime_proxy_inserts_pending_and_rejects_duplicates() {
        let manager = make_manager(vec![ip(1)]);

        assert!(manager.register_runtime_proxy(ip(2)));
        assert!(!manager.register_runtime_proxy(ip(2)));
        assert!(!manager.register_runtime_proxy(ip(1)));
        assert_eq!(
            manager.endpoints.get(&ip(2)).unwrap().state(),
            EndpointState::Pending
        );
        assert_eq!(
            manager.endpoints.get(&ip(2)).unwrap().source(),
            CandidateSource::UserConfigured
        );
    }

    #[test]
    fn extra_candidates_include_runtime_proxies() {
        let manager = make_manager(vec![ip(1)]);
        manager.register_runtime_proxy(ip(2));

        let extras = manager.extra_candidates();
        let candidates = collect_candidates(&[], &extras);

        assert_eq!(
            candidates,
            vec![
                (ip(1), CandidateSource::UserConfigured),
                (ip(2), CandidateSource::UserConfigured),
            ]
        );
    }

    #[test]
    fn existing_endpoint_states_are_not_reset_by_candidates() {
        let manager = make_manager(vec![ip(1), ip(2)]);

        manager.endpoints.insert(
            ip(1),
            SubstituterEndpoint::new(ip(1), CandidateSource::UserConfigured)
                .on_admission_success(Duration::from_millis(50)),
        );

        for (candidate, source) in collect_candidates(&[], &manager.user_candidates) {
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
    }

    #[test]
    fn report_failure_transitions_state() {
        let manager = make_manager(vec![]);
        manager.endpoints.insert(
            ip(1),
            SubstituterEndpoint::new(ip(1), CandidateSource::SniProxy)
                .on_admission_success(Duration::from_millis(50)),
        );
        manager.endpoints.insert(
            ip(2),
            SubstituterEndpoint::new(ip(2), CandidateSource::SniProxy)
                .on_admission_success(Duration::from_millis(60)),
        );

        manager.report_failure(ip(1), EndpointFailureKind::Transient);
        manager.report_failure(ip(2), EndpointFailureKind::Certificate);
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
        let manager = make_manager(vec![]);
        manager.endpoints.insert(
            ip(1),
            SubstituterEndpoint::new(ip(1), CandidateSource::SniProxy),
        );

        assert!(manager.client_for(ip(1)).is_some());
        assert!(manager.client_for(ip(2)).is_none());
    }

    #[test]
    fn snapshot_orders_usable_by_latency_then_pending_cooling_incompatible() {
        let manager = make_manager(vec![]);
        let now = Instant::now();
        let insert = |octet: u8, state: EndpointState| {
            manager.endpoints.insert(
                ip(octet),
                SubstituterEndpoint::new(ip(octet), CandidateSource::SniProxy).with_state(state),
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
                bandwidth: None,
            },
        );
        insert(
            2,
            EndpointState::Usable {
                admission_latency: Duration::from_millis(50),
                bandwidth: None,
            },
        );

        let snapshot = manager.snapshot();

        let ips: Vec<IpAddr> = snapshot.iter().map(|s| s.ip).collect();
        assert_eq!(ips, vec![ip(2), ip(1), ip(5), ip(3), ip(4)]);
        assert_eq!(
            snapshot[0].status,
            EndpointSnapshotStatus::Usable {
                admission_latency: Duration::from_millis(50),
                bandwidth: None,
            }
        );
        assert_eq!(snapshot[2].status, EndpointSnapshotStatus::Pending);
        assert_eq!(snapshot[3].status, EndpointSnapshotStatus::Cooling);
        assert_eq!(snapshot[4].status, EndpointSnapshotStatus::Incompatible);
    }

    #[test]
    fn update_selected_tracks_the_first_choice_endpoint() {
        let manager = make_manager(vec![]);

        manager.update_selected();
        assert_eq!(*manager.last_selected.lock().unwrap(), None);

        manager.endpoints.insert(
            ip(1),
            SubstituterEndpoint::new(ip(1), CandidateSource::SniProxy)
                .on_admission_success(Duration::from_millis(100)),
        );
        manager.update_selected();
        assert_eq!(*manager.last_selected.lock().unwrap(), Some(ip(1)));

        manager.report_failure(ip(1), EndpointFailureKind::Transient);
        manager.update_selected();
        assert_eq!(*manager.last_selected.lock().unwrap(), None);
    }
}
