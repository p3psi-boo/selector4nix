use std::net::IpAddr;
use std::time::Duration;

use tokio::time::Instant;

/// Hosts eligible for Fastly endpoint optimization. The endpoint mechanism is
/// generic, but only these hosts may enable it.
pub const FASTLY_OPTIMIZATION_HOSTS: &[&str] = &["cache.nixos.org"];

/// Cooling period after a transient endpoint failure before it may be probed again.
pub const ENDPOINT_COOLING_PERIOD: Duration = Duration::from_secs(300);

pub fn is_fastly_optimization_host(host: &str) -> bool {
    FASTLY_OPTIMIZATION_HOSTS.contains(&host)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointOptimizationKind {
    Fastly,
    Cloudflare,
}

/// Determines which kind of endpoint optimization applies to a substituter
/// host (independent of whether it is enabled; enablement lives in the
/// configuration layer). Fastly: exact match against
/// FASTLY_OPTIMIZATION_HOSTS; Cloudflare: host is `cachix.org` or ends with
/// `.cachix.org`.
pub fn endpoint_optimization_kind(host: &str) -> Option<EndpointOptimizationKind> {
    if is_fastly_optimization_host(host) {
        Some(EndpointOptimizationKind::Fastly)
    } else if host == "cachix.org" || host.ends_with(".cachix.org") {
        Some(EndpointOptimizationKind::Cloudflare)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateSource {
    /// Resolved via DNS-over-HTTPS at runtime.
    DnsDoh,
    /// Explicitly listed in the configuration.
    UserConfigured,
    /// Derived from another candidate via Fastly region patterns.
    DerivedRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointFailureKind {
    /// TCP connect/timeout/reset: transient, eligible again after cooling.
    Transient,
    /// TLS certificate validation failed for the logical host: permanent
    /// incompatibility, never retried.
    Certificate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    /// Discovered but not yet admission-probed.
    Pending,
    /// Passed TLS + HTTP admission; carries the admission request latency.
    Usable { admission_latency: Duration },
    /// Transient failure; excluded from selection until the given instant.
    Cooling { until: Instant },
    /// Failed TLS admission; permanently excluded.
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstituterEndpoint {
    ip: IpAddr,
    source: CandidateSource,
    state: EndpointState,
}

impl SubstituterEndpoint {
    pub fn new(ip: IpAddr, source: CandidateSource) -> Self {
        Self {
            ip,
            source,
            state: EndpointState::Pending,
        }
    }

    pub fn ip(&self) -> IpAddr {
        self.ip
    }

    pub fn source(&self) -> CandidateSource {
        self.source
    }

    pub fn state(&self) -> EndpointState {
        self.state
    }

    pub fn with_state(&self, state: EndpointState) -> Self {
        Self {
            state,
            ..self.clone()
        }
    }

    pub fn on_admission_success(&self, latency: Duration) -> Self {
        self.with_state(EndpointState::Usable {
            admission_latency: latency,
        })
    }

    pub fn on_failure(&self, kind: EndpointFailureKind, now: Instant) -> Self {
        match kind {
            EndpointFailureKind::Transient => self.with_state(EndpointState::Cooling {
                until: now + ENDPOINT_COOLING_PERIOD,
            }),
            EndpointFailureKind::Certificate => self.with_state(EndpointState::Incompatible),
        }
    }

    pub fn is_usable(&self, now: Instant) -> bool {
        match self.state {
            EndpointState::Usable { .. } => true,
            EndpointState::Cooling { until } => now >= until,
            EndpointState::Pending | EndpointState::Incompatible => false,
        }
    }
}

/// Point-in-time view of one endpoint for observability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointSnapshot {
    pub ip: IpAddr,
    pub source: CandidateSource,
    pub status: EndpointSnapshotStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSnapshotStatus {
    Pending,
    Usable { admission_latency: Duration },
    Cooling,
    Incompatible,
}

impl EndpointSnapshot {
    pub fn of(endpoint: &SubstituterEndpoint) -> Self {
        let status = match endpoint.state() {
            EndpointState::Pending => EndpointSnapshotStatus::Pending,
            EndpointState::Usable { admission_latency } => {
                EndpointSnapshotStatus::Usable { admission_latency }
            }
            EndpointState::Cooling { .. } => EndpointSnapshotStatus::Cooling,
            EndpointState::Incompatible => EndpointSnapshotStatus::Incompatible,
        };
        Self {
            ip: endpoint.ip(),
            source: endpoint.source(),
            status,
        }
    }
}

/// Order endpoints for selection: usable ones first by admission latency,
/// then pending ones (not yet probed), excluding cooling and incompatible.
pub fn order_for_selection(
    endpoints: &[SubstituterEndpoint],
    now: Instant,
) -> Vec<SubstituterEndpoint> {
    let mut usable: Vec<_> = endpoints
        .iter()
        .filter(|e| e.is_usable(now))
        .cloned()
        .collect();
    usable.sort_by_key(|e| match e.state() {
        EndpointState::Usable { admission_latency } => admission_latency,
        _ => Duration::MAX,
    });
    usable
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn ep(octet: u8, state: EndpointState) -> SubstituterEndpoint {
        SubstituterEndpoint::new(
            IpAddr::V4(Ipv4Addr::new(151, 101, 1, octet)),
            CandidateSource::DnsDoh,
        )
        .with_state(state)
    }

    #[test]
    fn usable_endpoints_are_ordered_by_admission_latency() {
        let now = Instant::now();
        let endpoints = vec![
            ep(
                1,
                EndpointState::Usable {
                    admission_latency: Duration::from_millis(300),
                },
            ),
            ep(
                2,
                EndpointState::Usable {
                    admission_latency: Duration::from_millis(100),
                },
            ),
            ep(
                3,
                EndpointState::Cooling {
                    until: now + Duration::from_secs(60),
                },
            ),
            ep(4, EndpointState::Incompatible),
            ep(5, EndpointState::Pending),
        ];

        let ordered = order_for_selection(&endpoints, now);
        let ips: Vec<_> = ordered.iter().map(|e| e.ip()).collect();
        assert_eq!(
            ips,
            vec![
                IpAddr::V4(Ipv4Addr::new(151, 101, 1, 2)),
                IpAddr::V4(Ipv4Addr::new(151, 101, 1, 1)),
            ]
        );
    }

    #[test]
    fn cooling_endpoint_becomes_usable_after_expiry() {
        let now = Instant::now();
        let endpoint = ep(1, EndpointState::Cooling { until: now });
        assert!(endpoint.is_usable(now));
    }

    #[test]
    fn transient_failure_cools_down_certificate_failure_excludes() {
        let now = Instant::now();
        let endpoint = ep(
            1,
            EndpointState::Usable {
                admission_latency: Duration::from_millis(100),
            },
        );

        let cooled = endpoint.on_failure(EndpointFailureKind::Transient, now);
        assert!(!cooled.is_usable(now));
        assert!(matches!(cooled.state(), EndpointState::Cooling { .. }));

        let incompatible = endpoint.on_failure(EndpointFailureKind::Certificate, now);
        assert_eq!(incompatible.state(), EndpointState::Incompatible);
        assert!(!incompatible.is_usable(now + ENDPOINT_COOLING_PERIOD));
    }

    #[test]
    fn only_cache_nixos_org_is_eligible() {
        assert!(is_fastly_optimization_host("cache.nixos.org"));
        assert!(!is_fastly_optimization_host("other.example.com"));
    }

    #[test]
    fn endpoint_optimization_kind_matches_fastly_hosts() {
        assert_eq!(
            endpoint_optimization_kind("cache.nixos.org"),
            Some(EndpointOptimizationKind::Fastly)
        );
    }

    #[test]
    fn endpoint_optimization_kind_matches_cachix_hosts() {
        assert_eq!(
            endpoint_optimization_kind("nix-community.cachix.org"),
            Some(EndpointOptimizationKind::Cloudflare)
        );
        assert_eq!(
            endpoint_optimization_kind("foo.cachix.org"),
            Some(EndpointOptimizationKind::Cloudflare)
        );
        assert_eq!(
            endpoint_optimization_kind("cachix.org"),
            Some(EndpointOptimizationKind::Cloudflare)
        );
    }

    #[test]
    fn endpoint_optimization_kind_rejects_other_hosts() {
        assert_eq!(endpoint_optimization_kind("cachix.org.evil.com"), None);
        assert_eq!(endpoint_optimization_kind("notcachix.org"), None);
        assert_eq!(endpoint_optimization_kind("releases.nixos.org"), None);
    }
}
