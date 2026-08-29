//! Admission probing of endpoint candidates (TLS + /nix-cache-info).

use std::error::Error as _;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use http::{StatusCode, header};
use snafu::Snafu;

use super::endpoint_client_pool::EndpointClientPool;
use crate::domain::common::url::Url;
use crate::domain::substituter::model::BandwidthMeasurement;
use crate::infrastructure::config::BandwidthProbeConfiguration;

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

    /// Download a bounded byte range of a configured, known-large NAR file
    /// through one endpoint. This is a performance measurement only: a failed
    /// benchmark never makes an otherwise admitted endpoint unavailable.
    pub async fn benchmark_endpoint(
        &self,
        base_url: &Url,
        ip: IpAddr,
        config: &BandwidthProbeConfiguration,
    ) -> Result<BandwidthMeasurement, ProbeEndpointError> {
        let url = base_url.as_dir().join(&config.nar_path).unwrap();
        let end = config.bytes.get() - 1;
        let client = &self.pool.get_or_build(ip).http;
        let started = Instant::now();

        let response = client
            .get(url.value())
            .header(header::RANGE, format!("bytes=0-{end}"))
            .header(header::ACCEPT_ENCODING, "identity")
            .timeout(self.default_timeout)
            .send()
            .await
            .map_err(|error| classify_error(ip, &error))?;

        if response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(ProbeEndpointError::Transient {
                ip,
                message: format!(
                    "bandwidth benchmark expected HTTP 206 from {url}, got {}",
                    response.status()
                ),
            });
        }

        let mut body = response.bytes_stream();
        let Some(first) = body.next().await else {
            return Err(ProbeEndpointError::Transient {
                ip,
                message: format!("bandwidth benchmark received an empty body from {url}"),
            });
        };
        let first = first.map_err(|error| ProbeEndpointError::Transient {
            ip,
            message: format!("failed to read benchmark body from {url}: {error}"),
        })?;
        let time_to_first_byte = started.elapsed();
        let mut downloaded = first.len();

        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|error| ProbeEndpointError::Transient {
                ip,
                message: format!("failed to read benchmark body from {url}: {error}"),
            })?;
            downloaded = downloaded.saturating_add(chunk.len());
            if downloaded > config.bytes.get() {
                return Err(ProbeEndpointError::Transient {
                    ip,
                    message: format!("bandwidth benchmark received more than requested from {url}"),
                });
            }
        }

        if downloaded != config.bytes.get() {
            return Err(ProbeEndpointError::Transient {
                ip,
                message: format!(
                    "bandwidth benchmark downloaded {downloaded} bytes from {url}, expected {}",
                    config.bytes
                ),
            });
        }

        let total_elapsed = started.elapsed();
        let transfer_elapsed = total_elapsed
            .checked_sub(time_to_first_byte)
            .unwrap_or(Duration::ZERO)
            .max(Duration::from_nanos(1));
        let bytes_per_second = ((downloaded as u128)
            .saturating_mul(1_000_000_000)
            .checked_div(transfer_elapsed.as_nanos())
            .unwrap_or(u128::from(u64::MAX)))
        .min(u128::from(u64::MAX)) as u64;
        let measurement = BandwidthMeasurement {
            time_to_first_byte,
            bytes_per_second,
            sampled_at: tokio::time::Instant::now(),
        };
        tracing::debug!(%ip, %url, downloaded, ?measurement, "endpoint bandwidth benchmark completed");
        Ok(measurement)
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

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, TcpListener};
    use std::num::NonZeroUsize;

    use reqwest::Client;
    use selector4nix_streaming::throttler::{PerHostHttpThrottler, ThrottlingOptions};

    use super::*;
    use crate::infrastructure::config::BandwidthProbeConfiguration;

    #[tokio::test]
    async fn bandwidth_probe_reads_exact_requested_range() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let length = socket.read(&mut request).unwrap();
            let request = std::str::from_utf8(&request[..length]).unwrap();
            assert!(request.contains("GET /nar/probe.nar.zst HTTP/1.1"));
            assert!(request.contains("range: bytes=0-15") || request.contains("Range: bytes=0-15"));
            socket
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\n\
                      content-length: 16\r\n\
                      content-range: bytes 0-15/32\r\n\
                      connection: close\r\n\
                      \r\n\
                      0123456789abcdef",
                )
                .unwrap();
        });

        let pool = Arc::new(EndpointClientPool::new(
            "cache.nixos.org".to_string(),
            port,
            Arc::new(Client::builder),
            Arc::new(PerHostHttpThrottler::new(ThrottlingOptions::new(
                NonZeroUsize::new(1).unwrap(),
            ))),
            false,
            NonZeroUsize::new(1024).unwrap(),
            NonZeroUsize::new(1).unwrap(),
            1,
        ));
        let provider = EndpointProbingProvider::new(Arc::clone(&pool), Duration::from_secs(5));
        let config = BandwidthProbeConfiguration {
            enabled: true,
            nar_path: "nar/probe.nar.zst".to_string(),
            bytes: NonZeroUsize::new(16).unwrap(),
            refresh_interval: Duration::from_secs(60),
            max_concurrent_probes: NonZeroUsize::new(1).unwrap(),
        };

        let measurement = provider
            .benchmark_endpoint(
                &Url::new(&format!("http://cache.nixos.org:{port}")).unwrap(),
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                &config,
            )
            .await
            .unwrap();

        assert!(measurement.bytes_per_second > 0);
        server.join().unwrap();
    }
}
