use std::error::Error as _;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Error as AnyhowError;
use async_trait::async_trait;
use http::StatusCode;
use reqwest::{Client, RequestBuilder, Response};
use snafu::ResultExt;

use crate::domain::common::passthrough_headers::PassthroughHeaders;
use crate::domain::common::url::Url;
use crate::domain::nar_info::model::UpstreamNarInfoData;
use crate::domain::nar_info::port::error_ctx::{OfflineSnafu, ServiceSnafu};
use crate::domain::nar_info::port::{NarInfoProvider, NarInfoQueryData, QueryNarInfoError};
use crate::domain::substituter::model::EndpointFailureKind;
use crate::infrastructure::config::AppCredential;
use crate::infrastructure::endpoint::manager::EndpointManager;
use crate::infrastructure::endpoint::registry::EndpointManagerRegistry;

/// The manager and its ordered usable endpoint IPs when a manager is
/// registered for `host` and it has usable endpoints. `None` means the caller
/// falls back to the default client path.
pub(crate) fn endpoint_ips_for(
    host: &str,
    registry: &EndpointManagerRegistry,
) -> Option<(Arc<EndpointManager>, Vec<IpAddr>)> {
    let manager = registry.for_host(host)?;
    let ips = manager.ordered_usable();
    if ips.is_empty() {
        None
    } else {
        Some((manager, ips))
    }
}

/// reqwest 0.13 does not expose a dedicated TLS-certificate error kind, so
/// the error chain is scanned for certificate-related keywords (mirrors the
/// admission-probing classification in `EndpointProbingProvider`).
pub(crate) fn classify_endpoint_failure(error: &reqwest::Error) -> EndpointFailureKind {
    let mut looks_like_certificate = contains_certificate_keyword(&error.to_string());
    let mut source = error.source();
    while let Some(cause) = source {
        if !looks_like_certificate && contains_certificate_keyword(&cause.to_string()) {
            looks_like_certificate = true;
        }
        source = cause.source();
    }
    if looks_like_certificate {
        EndpointFailureKind::Certificate
    } else {
        EndpointFailureKind::Transient
    }
}

fn contains_certificate_keyword(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("certificate") || lower.contains("ssl") || lower.contains("tls")
}

pub struct ReqwestNarInfoProvider {
    client: Client,
    default_timeout: Duration,
    credentials: Arc<AppCredential>,
    endpoint_managers: EndpointManagerRegistry,
}

impl ReqwestNarInfoProvider {
    pub fn new(
        client: Client,
        default_timeout: Duration,
        credentials: Arc<AppCredential>,
        endpoint_managers: EndpointManagerRegistry,
    ) -> Self {
        Self {
            client,
            default_timeout,
            credentials,
            endpoint_managers,
        }
    }

    fn build_request(
        &self,
        client: &Client,
        url: &Url,
        headers: &PassthroughHeaders,
        timeout: Duration,
    ) -> RequestBuilder {
        let request = client
            .get(url.value())
            .headers(headers.to_headers())
            .timeout(timeout);

        // Credentials are looked up by the logical URL regardless of which
        // endpoint client carries the request.
        if let Some(credential) = self.credentials.lookup(url) {
            request.basic_auth(credential.login.clone(), credential.secret.clone())
        } else {
            request
        }
    }

    async fn handle_response(
        url: &Url,
        start: Instant,
        response: Response,
    ) -> Result<Option<NarInfoQueryData>, QueryNarInfoError> {
        match response.status() {
            StatusCode::OK => {
                let text = (response.text().await)
                    .map_err(|err| AnyhowError::new(err))
                    .map_err(|err| err.context(format!("failed to read nar info body from {url}")))
                    .context(ServiceSnafu)
                    .inspect_err(|_| tracing::debug!(%url, "failed to read nar info body"))?;
                let latency = start.elapsed();
                let original_data = UpstreamNarInfoData::new(text)
                    .map_err(|err| AnyhowError::new(err))
                    .map_err(|err| err.context(format!("invalid nar info from {url}")))
                    .context(ServiceSnafu)
                    .inspect_err(|_| tracing::debug!(%url, "failed to parse nar info body"))?;
                tracing::debug!(%url, "fetched nar info from substituter");
                Ok(Some(NarInfoQueryData::new(original_data, latency)))
            }
            StatusCode::NOT_FOUND | StatusCode::FORBIDDEN => Ok(None),
            status => Err(anyhow::anyhow!("unexpected status {} from {}", status, url))
                .context(ServiceSnafu)
                .inspect_err(|_| tracing::debug!(%url, "encountered bad nar info response status")),
        }
    }
}

#[async_trait]
impl NarInfoProvider for ReqwestNarInfoProvider {
    async fn query_nar_info(
        &self,
        url: &Url,
        headers: &PassthroughHeaders,
        timeout: Option<Duration>,
    ) -> Result<Option<NarInfoQueryData>, QueryNarInfoError> {
        tracing::debug!(%url, "fetching nar info from substituter");

        let timeout = timeout.unwrap_or(self.default_timeout);

        if let Some((manager, ips)) = endpoint_ips_for(url.host(), &self.endpoint_managers) {
            for ip in ips {
                let Some(clients) = manager.client_for(ip) else {
                    continue;
                };
                tracing::trace!(%url, %ip, "querying nar info via endpoint");
                let request = self.build_request(&clients.http, url, headers, timeout);
                let start = Instant::now();
                match request.send().await {
                    // A completed HTTP exchange (including 404/403: the
                    // resource is absent, the endpoint is healthy) is final.
                    Ok(response) => return Self::handle_response(url, start, response).await,
                    Err(err) => {
                        let kind = classify_endpoint_failure(&err);
                        tracing::debug!(%url, %ip, ?kind, error = %err, "endpoint nar info query failed; trying next endpoint");
                        manager.report_failure(ip, kind);
                    }
                }
            }
            tracing::warn!(%url, "all endpoints failed; falling back to default client");
        }

        let request = self.build_request(&self.client, url, headers, timeout);

        let start = Instant::now();
        let response = match request.send().await {
            Ok(response) => response,
            Err(err) => {
                tracing::debug!(%url, is_timeout = %err.is_timeout(), "failed to send nar info query request");
                if err.is_timeout() || err.is_connect() || err.is_request() {
                    return Err(AnyhowError::new(err)).context(OfflineSnafu);
                } else {
                    return Err(AnyhowError::new(err)).context(ServiceSnafu);
                }
            }
        };

        Self::handle_response(url, start, response).await
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, TcpListener};
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use reqwest::Client;
    use selector4nix_streaming::throttler::{PerHostHttpThrottler, ThrottlingOptions};

    use super::*;
    use crate::infrastructure::provider::{
        EndpointClientPool, EndpointProbingProvider, SniProxySourceProvider,
    };

    fn make_manager(user_candidates: Vec<IpAddr>, port: u16) -> EndpointManager {
        let pool = Arc::new(EndpointClientPool::new(
            "cache.nixos.org".to_string(),
            port,
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
            Url::new(&format!("http://cache.nixos.org:{port}")).unwrap(),
            pool,
            probing,
            user_candidates,
            Vec::new(),
            Arc::new(SniProxySourceProvider::new()),
            None,
        )
    }

    #[test]
    fn non_whitelist_host_falls_back() {
        assert!(
            endpoint_ips_for("releases.nixos.org", &EndpointManagerRegistry::default()).is_none()
        );
    }

    #[test]
    fn missing_manager_falls_back() {
        assert!(endpoint_ips_for("cache.nixos.org", &EndpointManagerRegistry::default()).is_none());
    }

    #[test]
    fn empty_usable_endpoints_fall_back() {
        let manager = make_manager(Vec::new(), 1443);
        let registry = EndpointManagerRegistry::new(vec![Arc::new(manager)]);
        assert!(endpoint_ips_for("cache.nixos.org", &registry).is_none());
    }

    /// Serve fixed HTTP 200 responses until `stop` is raised.
    fn spawn_ok_server(listener: TcpListener, stop: Arc<AtomicBool>) {
        listener.set_nonblocking(true).unwrap();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        std::thread::spawn(move || {
                            let _ = socket.set_nonblocking(false);
                            let mut buffer = [0u8; 1024];
                            let _ = socket.read(&mut buffer);
                            let _ = socket.write_all(
                                b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                            );
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
    }

    #[tokio::test]
    async fn whitelisted_host_returns_admitted_endpoints_and_failures_cool_down() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        spawn_ok_server(listener, Arc::clone(&stop));

        let ip1 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let manager = Arc::new(make_manager(vec![ip1], port));
        manager.refresh().await;
        let registry = EndpointManagerRegistry::new(vec![Arc::clone(&manager)]);

        let (_, ips) = endpoint_ips_for("cache.nixos.org", &registry)
            .expect("admitted endpoints should be usable");
        assert_eq!(ips, vec![ip1]);

        // Kill the admission server: subsequent endpoint requests fail fast
        // with connect-refused and must cool the endpoints down.
        stop.store(true, Ordering::Relaxed);
        // Give the accept loop a moment to drop the listener and free the port.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let provider = ReqwestNarInfoProvider::new(
            Client::new(),
            Duration::from_millis(1500),
            Arc::new(AppCredential::empty()),
            registry,
        );
        let url = Url::new(&format!("http://cache.nixos.org:{port}/deadbeef.narinfo")).unwrap();

        // The endpoints fail; the default-client fallback also fails (the
        // logical host does not resolve here), so the query errors out while
        // preserving the error path.
        let result = provider
            .query_nar_info(
                &url,
                &PassthroughHeaders::empty(),
                Some(Duration::from_millis(1500)),
            )
            .await;
        assert!(result.is_err());

        let manager = provider
            .endpoint_managers
            .for_host("cache.nixos.org")
            .expect("the fastly manager is registered");
        assert!(
            manager.ordered_usable().is_empty(),
            "failed endpoints must be cooling and no longer usable"
        );
    }
}
