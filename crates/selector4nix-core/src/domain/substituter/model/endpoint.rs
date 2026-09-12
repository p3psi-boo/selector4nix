use std::net::IpAddr;
use std::time::Duration;

use tokio::time::Instant;

/// Cooling period after a transient endpoint failure before it may be probed again.
pub const ENDPOINT_COOLING_PERIOD: Duration = Duration::from_secs(300);

/// CDN platform whose SNI proxy lists and bandwidth probe configuration
/// apply to a substituter. The platform is detected at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointOptimizationKind {
    Fastly,
    Cloudflare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateSource {
    /// Loaded from a platform-specific local or remote SNI proxy list.
    SniProxy,
    /// Explicitly listed in the configuration as an extra SNI proxy IP.
    UserConfigured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointFailureKind {
    /// TCP connect/timeout/reset: transient, eligible again after cooling.
    Transient,
    /// TLS certificate validation failed for the logical host: permanent
    /// incompatibility, never retried.
    Certificate,
}

/// A bounded Range-download measurement for an admitted endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandwidthMeasurement {
    pub time_to_first_byte: Duration,
    pub bytes_per_second: u64,
    pub sampled_at: Instant,
}

impl BandwidthMeasurement {
    /// Estimate the time needed to transfer `bytes` through this endpoint.
    /// The estimate includes TTFB so that a high-bandwidth but high-latency
    /// endpoint does not dominate small NAR downloads.
    pub fn estimated_download_time(&self, bytes: usize) -> Duration {
        let bytes_per_second = u128::from(self.bytes_per_second.max(1));
        let transfer_nanos = (bytes as u128)
            .saturating_mul(1_000_000_000)
            .checked_div(bytes_per_second)
            .unwrap_or(u128::from(u64::MAX))
            .min(u128::from(u64::MAX));
        self.time_to_first_byte
            .saturating_add(Duration::from_nanos(transfer_nanos as u64))
    }

    /// Blend an observation from a real NAR transfer into this estimate.
    /// History carries 80% of the weight to avoid route churn from bursts.
    pub fn merge_observation(&self, observation: Self) -> Self {
        const HISTORY_WEIGHT: u128 = 4;
        const TOTAL_WEIGHT: u128 = 5;

        let weighted_nanos = self
            .time_to_first_byte
            .as_nanos()
            .saturating_mul(HISTORY_WEIGHT)
            .saturating_add(observation.time_to_first_byte.as_nanos())
            / TOTAL_WEIGHT;
        let bytes_per_second = (u128::from(self.bytes_per_second)
            .saturating_mul(HISTORY_WEIGHT)
            .saturating_add(u128::from(observation.bytes_per_second))
            / TOTAL_WEIGHT)
            .min(u128::from(u64::MAX)) as u64;

        Self {
            time_to_first_byte: Duration::from_nanos(
                weighted_nanos.min(u128::from(u64::MAX)) as u64
            ),
            bytes_per_second,
            sampled_at: observation.sampled_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointState {
    /// Discovered but not yet admission-probed.
    Pending,
    /// Passed TLS + HTTP admission. The optional measurement is recorded by
    /// the active Range-download benchmark.
    Usable {
        admission_latency: Duration,
        bandwidth: Option<BandwidthMeasurement>,
    },
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
        let bandwidth = match self.state {
            EndpointState::Usable { bandwidth, .. } => bandwidth,
            _ => None,
        };
        self.with_state(EndpointState::Usable {
            admission_latency: latency,
            bandwidth,
        })
    }

    pub fn on_bandwidth_success(&self, measurement: BandwidthMeasurement) -> Self {
        match self.state {
            EndpointState::Usable {
                admission_latency, ..
            } => self.with_state(EndpointState::Usable {
                admission_latency,
                bandwidth: Some(measurement),
            }),
            _ => self.clone(),
        }
    }

    pub fn on_download_observed(&self, observation: BandwidthMeasurement) -> Self {
        match self.state {
            EndpointState::Usable {
                admission_latency,
                bandwidth,
            } => self.with_state(EndpointState::Usable {
                admission_latency,
                bandwidth: Some(match bandwidth {
                    Some(previous) => previous.merge_observation(observation),
                    None => observation,
                }),
            }),
            _ => self.clone(),
        }
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

    pub fn needs_bandwidth_probe(&self, now: Instant, refresh_interval: Duration) -> bool {
        match self.state {
            EndpointState::Usable {
                bandwidth: Some(measurement),
                ..
            } => now.duration_since(measurement.sampled_at) >= refresh_interval,
            EndpointState::Usable {
                bandwidth: None, ..
            } => true,
            _ => false,
        }
    }
}

/// Point-in-time view of one endpoint for observability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointSnapshot {
    pub ip: IpAddr,
    pub source: CandidateSource,
    pub status: EndpointSnapshotStatus,
    pub retry_after_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointSnapshotStatus {
    Pending,
    Usable {
        admission_latency: Duration,
        bandwidth: Option<BandwidthMeasurement>,
    },
    Cooling,
    Incompatible,
}

impl EndpointSnapshot {
    pub fn of(endpoint: &SubstituterEndpoint) -> Self {
        let status = match endpoint.state() {
            EndpointState::Pending => EndpointSnapshotStatus::Pending,
            EndpointState::Usable {
                admission_latency,
                bandwidth,
            } => EndpointSnapshotStatus::Usable {
                admission_latency,
                bandwidth,
            },
            EndpointState::Cooling { .. } => EndpointSnapshotStatus::Cooling,
            EndpointState::Incompatible => EndpointSnapshotStatus::Incompatible,
        };
        Self {
            retry_after_secs: match endpoint.state() {
                EndpointState::Cooling { until } => {
                    Some(until.saturating_duration_since(Instant::now()).as_secs())
                }
                _ => None,
            },
            ip: endpoint.ip(),
            source: endpoint.source(),
            status,
        }
    }
}

/// Order usable endpoints by their 10 MiB estimated download time when a
/// bandwidth sample exists, falling back to admission latency otherwise.
/// Cooling and incompatible endpoints are excluded.
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
        EndpointState::Usable {
            admission_latency: _,
            bandwidth: Some(measurement),
        } => (0, measurement.estimated_download_time(10 * 1024 * 1024)),
        EndpointState::Usable {
            admission_latency,
            bandwidth: None,
        } => (1, admission_latency),
        _ => (2, Duration::MAX),
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
            CandidateSource::SniProxy,
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
                    bandwidth: None,
                },
            ),
            ep(
                2,
                EndpointState::Usable {
                    admission_latency: Duration::from_millis(100),
                    bandwidth: None,
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
                bandwidth: None,
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
    fn measured_endpoint_is_ordered_by_estimated_download_time() {
        let now = Instant::now();
        let fast_bandwidth = BandwidthMeasurement {
            time_to_first_byte: Duration::from_millis(80),
            bytes_per_second: 100 * 1024 * 1024,
            sampled_at: now,
        };
        let endpoints = vec![
            ep(
                1,
                EndpointState::Usable {
                    admission_latency: Duration::from_millis(20),
                    bandwidth: None,
                },
            ),
            ep(
                2,
                EndpointState::Usable {
                    admission_latency: Duration::from_millis(100),
                    bandwidth: Some(fast_bandwidth),
                },
            ),
        ];

        let ordered = order_for_selection(&endpoints, now);
        assert_eq!(ordered[0].ip(), IpAddr::V4(Ipv4Addr::new(151, 101, 1, 2)));
    }

    #[test]
    fn bandwidth_probe_is_required_only_for_missing_or_expired_measurements() {
        let now = Instant::now();
        let fresh = ep(
            1,
            EndpointState::Usable {
                admission_latency: Duration::from_millis(10),
                bandwidth: Some(BandwidthMeasurement {
                    time_to_first_byte: Duration::from_millis(10),
                    bytes_per_second: 1,
                    sampled_at: now,
                }),
            },
        );
        assert!(!fresh.needs_bandwidth_probe(now, Duration::from_secs(60)));
        assert!(
            fresh.needs_bandwidth_probe(now + Duration::from_secs(60), Duration::from_secs(60))
        );
    }

    #[test]
    fn real_download_observations_are_smoothed() {
        let now = Instant::now();
        let previous = BandwidthMeasurement {
            time_to_first_byte: Duration::from_millis(100),
            bytes_per_second: 10_000,
            sampled_at: now,
        };
        let observation = BandwidthMeasurement {
            time_to_first_byte: Duration::from_millis(200),
            bytes_per_second: 20_000,
            sampled_at: now + Duration::from_secs(1),
        };

        let merged = previous.merge_observation(observation);

        assert_eq!(merged.time_to_first_byte, Duration::from_millis(120));
        assert_eq!(merged.bytes_per_second, 12_000);
        assert_eq!(merged.sampled_at, observation.sampled_at);
    }
}
