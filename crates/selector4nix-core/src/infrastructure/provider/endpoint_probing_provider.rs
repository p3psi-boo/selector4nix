//! Admission probing of endpoint candidates (TLS + /nix-cache-info).

use std::error::Error as _;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use snafu::Snafu;

use super::endpoint_client_pool::EndpointClientPool;
use crate::domain::common::url::Url;

#[derive(Snafu, Debug)]
pub enum ProbeEndpointError {
    #[snafu(display("endpoint {ip} failed TLS certificate validation: {message}"))]
    Certificate { ip: IpAddr, message: String },
    #[snafu(display("endpoint {ip} failed admission probing: {message}"))]
    Transient { ip: IpAddr, message: String },
}

/// Admission-probes endpoint candidates: a successful probe requires a
/// TLS-verified 200 response from `/nix-cache-info` through the endpoint.
pub struct EndpointProbingProvider {
    pool: Arc<EndpointClientPool>,
    default_timeout: Duration,
}

impl EndpointProbingProvider {
    pub fn new(pool: Arc<EndpointClientPool>, default_timeout: Duration) -> Self {
        Self {
            pool,
            default_timeout,
        }
    }

    /// Probe `ip` by requesting `{base_url}/nix-cache-info` through the
    /// endpoint-bound client. Returns the total request latency on success.
    pub async fn probe_endpoint(
        &self,
        base_url: &Url,
        ip: IpAddr,
    ) -> Result<Duration, ProbeEndpointError> {
        tracing::debug!(%ip, %base_url, "admission-probing endpoint candidate");

        let url = base_url.as_dir().join("nix-cache-info").unwrap();
        let client = &self.pool.get_or_build(ip).http;
        let started = Instant::now();

        let response = client
            .get(url.value())
            .timeout(self.default_timeout)
            .send()
            .await
            .map_err(|error| classify_error(ip, &error))?;

        if !response.status().is_success() {
            return Err(ProbeEndpointError::Transient {
                ip,
                message: format!("unexpected status {} from {url}", response.status()),
            });
        }

        response
            .text()
            .await
            .map_err(|error| ProbeEndpointError::Transient {
                ip,
                message: format!("failed to read nix-cache-info: {error}"),
            })?;

        let latency = started.elapsed();
        tracing::debug!(%ip, %url, ?latency, "endpoint passed admission probing");
        Ok(latency)
    }
}

/// reqwest 0.13 does not expose a dedicated TLS-certificate error kind, so
/// the error chain is scanned for certificate-related keywords.
fn classify_error(ip: IpAddr, error: &reqwest::Error) -> ProbeEndpointError {
    let mut message = error.to_string();
    let mut looks_like_certificate = contains_certificate_keyword(&message);

    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !looks_like_certificate && contains_certificate_keyword(&text) {
            looks_like_certificate = true;
        }
        message.push_str(&format!(" (caused by: {text})"));
        source = cause.source();
    }

    // Keep the full chain in the logs so the heuristic can be calibrated
    // against real-world failures.
    tracing::debug!(%ip, %message, "endpoint admission probe failed");
    if looks_like_certificate {
        ProbeEndpointError::Certificate { ip, message }
    } else {
        ProbeEndpointError::Transient { ip, message }
    }
}

fn contains_certificate_keyword(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("certificate") || lower.contains("ssl") || lower.contains("tls")
}
