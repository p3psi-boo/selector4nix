use std::sync::Arc;
use std::time::Duration;

use anyhow::Error as AnyhowError;
use async_trait::async_trait;
use http::StatusCode;
use reqwest::{Client, Response};
use snafu::ResultExt;

use crate::domain::common::url::Url;
use crate::domain::substituter::model::SubstituterMeta;
use crate::domain::substituter::port::error_ctx::{OfflineSnafu, ServiceSnafu};
use crate::domain::substituter::port::{ProbeSubstituterError, SubstituterProbingProvider};
use crate::infrastructure::config::AppCredential;
use crate::infrastructure::endpoint::registry::EndpointManagerRegistry;
use crate::infrastructure::provider::nar_info_provider::{
    classify_endpoint_failure, endpoint_ips_for,
};

pub struct ReqwestSubstituterProbingProvider {
    client: Client,
    default_timeout: Duration,
    credentials: Arc<AppCredential>,
    endpoint_managers: EndpointManagerRegistry,
}

impl ReqwestSubstituterProbingProvider {
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

    /// Credentials are looked up by the logical URL regardless of which
    /// endpoint client carries the request.
    async fn send_probe(
        &self,
        client: &Client,
        url: &Url,
        timeout: Duration,
    ) -> Result<Response, reqwest::Error> {
        let request = client.get(url.value()).timeout(timeout);

        let request = if let Some(credential) = self.credentials.lookup(url) {
            request.basic_auth(credential.login.clone(), credential.secret.clone())
        } else {
            request
        };

        request.send().await
    }

    async fn handle_probe_response(
        url: &Url,
        response: Response,
    ) -> Result<(), ProbeSubstituterError> {
        match response.status() {
            StatusCode::OK => {
                let _ = (response.text().await)
                    .map_err(|err| AnyhowError::new(err))
                    .map_err(|err| err.context(format!("failed to read nix-cache-info from {url}")))
                    .context(ServiceSnafu)
                    .inspect_err(|_| tracing::debug!(%url, "failed to read nix-cache-info"))?;
                tracing::debug!(%url, "probed substituter successfully");
                Ok(())
            }
            status => Err(anyhow::anyhow!("unexpected status {} from {}", status, url))
                .context(ServiceSnafu)
                .inspect_err(
                    |_| tracing::debug!(%url, "encountered bad nix-cache-info response status"),
                ),
        }
    }
}

#[async_trait]
impl SubstituterProbingProvider for ReqwestSubstituterProbingProvider {
    async fn probe_substituter(
        &self,
        substituter: &SubstituterMeta,
    ) -> Result<(), ProbeSubstituterError> {
        tracing::debug!(substituter = %substituter.url(), "probing substituter's health status");

        let url = substituter.url().as_dir().join("nix-cache-info").unwrap();
        let timeout = substituter
            .nar_info_timeout()
            .unwrap_or(self.default_timeout);

        if let Some((manager, ips)) = endpoint_ips_for(url.host(), &self.endpoint_managers) {
            for ip in ips {
                let Some(clients) = manager.client_for(ip) else {
                    continue;
                };
                tracing::trace!(%url, %ip, "probing substituter via endpoint");
                match self.send_probe(&clients.http, &url, timeout).await {
                    // A completed HTTP exchange is final: the endpoint is
                    // healthy, the status decides the probe result.
                    Ok(response) => return Self::handle_probe_response(&url, response).await,
                    Err(err) => {
                        let kind = classify_endpoint_failure(&err);
                        tracing::debug!(%url, %ip, ?kind, error = %err, "endpoint probing request failed; trying next endpoint");
                        manager.report_failure(ip, kind);
                    }
                }
            }
            tracing::warn!(%url, "all endpoints failed; falling back to default client");
        }

        let response = match self.send_probe(&self.client, &url, timeout).await {
            Ok(response) => response,
            Err(err) => {
                tracing::debug!(%url, is_timeout = %err.is_timeout(), "failed to send probing request");
                if err.is_timeout() || err.is_connect() || err.is_request() {
                    return Err(AnyhowError::new(err)).context(OfflineSnafu);
                } else {
                    return Err(AnyhowError::new(err)).context(ServiceSnafu);
                }
            }
        };

        Self::handle_probe_response(&url, response).await
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, TcpListener};
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use reqwest::Client;
    use selector4nix_streaming::throttler::{PerHostHttpThrottler, ThrottlingOptions};

    use super::*;
    use crate::domain::substituter::model::Priority;
    use crate::infrastructure::dns::doh_resolver::DohResolver;
    use crate::infrastructure::endpoint::manager::EndpointManager;
    use crate::infrastructure::provider::{
        EndpointClientPool, EndpointProbingProvider, ExternalIpListProvider,
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
            Arc::new(DohResolver::new()),
            user_candidates,
            false,
            Vec::new(),
            Vec::new(),
            Arc::new(ExternalIpListProvider::new()),
            None,
        )
    }

    fn make_provider(port: u16, user_candidates: Vec<IpAddr>) -> ReqwestSubstituterProbingProvider {
        let manager = make_manager(user_candidates, port);
        let registry = EndpointManagerRegistry::new(vec![Arc::new(manager)]);
        ReqwestSubstituterProbingProvider::new(
            Client::new(),
            Duration::from_millis(1500),
            Arc::new(AppCredential::empty()),
            registry,
        )
    }

    fn make_substituter(port: u16) -> SubstituterMeta {
        SubstituterMeta::new(
            Url::new(&format!("http://cache.nixos.org:{port}")).unwrap(),
            Priority::new(40).unwrap(),
        )
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
    async fn empty_registry_uses_default_client() {
        let provider = ReqwestSubstituterProbingProvider::new(
            Client::new(),
            Duration::from_millis(1500),
            Arc::new(AppCredential::empty()),
            EndpointManagerRegistry::default(),
        );
        // The logical host does not resolve here, so the default path errors
        // out without touching any endpoint machinery.
        let result = provider.probe_substituter(&make_substituter(1443)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn usable_endpoint_probes_successfully() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        spawn_ok_server(listener, Arc::clone(&stop));

        let ip1 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let manager = Arc::new(make_manager(vec![ip1], port));
        manager.refresh().await;
        let registry = EndpointManagerRegistry::new(vec![manager]);

        let provider = ReqwestSubstituterProbingProvider::new(
            Client::new(),
            Duration::from_millis(1500),
            Arc::new(AppCredential::empty()),
            registry,
        );

        // The logical host does not resolve here; success can only come from
        // the endpoint path.
        provider
            .probe_substituter(&make_substituter(port))
            .await
            .expect("endpoint probe should succeed");

        stop.store(true, Ordering::Relaxed);
    }

    #[tokio::test]
    async fn failing_endpoints_cool_down_and_fall_back() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        spawn_ok_server(listener, Arc::clone(&stop));

        let ip1 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let manager = Arc::new(make_manager(vec![ip1], port));
        manager.refresh().await;
        let registry = EndpointManagerRegistry::new(vec![Arc::clone(&manager)]);

        // Kill the admission server: subsequent endpoint requests fail fast
        // with connect-refused and must cool the endpoints down.
        stop.store(true, Ordering::Relaxed);
        // Give the accept loop a moment to drop the listener and free the port.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let provider = ReqwestSubstituterProbingProvider::new(
            Client::new(),
            Duration::from_millis(1500),
            Arc::new(AppCredential::empty()),
            registry,
        );

        // The endpoints fail; the default-client fallback also fails (the
        // logical host does not resolve here), so the probe errors out while
        // preserving the error path.
        let result = provider.probe_substituter(&make_substituter(port)).await;
        assert!(result.is_err());

        assert!(
            manager.ordered_usable().is_empty(),
            "failed endpoints must be cooling and no longer usable"
        );
    }
}
