use std::error::Error as _;
use std::net::IpAddr;
use std::sync::Arc;

use anyhow::Result as AnyhowResult;
use async_trait::async_trait;
use http::header;
use selector4nix_streaming::{StreamHttpBodyError, StreamingClient, StreamingResponse};
use tokio::task::JoinSet;

use crate::domain::common::passthrough_headers::PassthroughHeaders;
use crate::domain::common::url::Url;
use crate::domain::nar_file::model::NarFileLocation;
use crate::domain::nar_file::port::{
    NarStreamData, NarStreamHeaders, NarStreamOpenAttempt, NarStreamProvider,
};
use crate::domain::substituter::model::EndpointFailureKind;
use crate::infrastructure::config::AppCredential;
use crate::infrastructure::endpoint::manager::EndpointManager;
use crate::infrastructure::endpoint::registry::EndpointManagerRegistry;

/// The manager and its ordered usable endpoint IPs when a manager is
/// registered for `host` and it has usable endpoints. `None` means the caller
/// falls back to the default client path.
fn endpoint_ips_for(
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
fn classify_endpoint_failure(error: &reqwest::Error) -> EndpointFailureKind {
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

pub struct ReqwestNarStreamProvider {
    client: Arc<StreamingClient>,
    credentials: Arc<AppCredential>,
    endpoint_managers: EndpointManagerRegistry,
}

impl ReqwestNarStreamProvider {
    pub fn new(
        client: Arc<StreamingClient>,
        credentials: Arc<AppCredential>,
        endpoint_managers: EndpointManagerRegistry,
    ) -> Self {
        Self {
            client,
            credentials,
            endpoint_managers,
        }
    }

    fn wrap_ok_response(
        url: Url,
        response: StreamingResponse,
    ) -> AnyhowResult<Option<NarStreamData>> {
        let headers = NarStreamHeaders {
            content_length: response.content_length(),
            content_type: response
                .raw_headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string),
            content_encoding: response
                .raw_headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string),
        };

        let stream = response.into_stream();
        Ok(Some(NarStreamData::new(headers, stream, url)))
    }
}

#[async_trait]
impl NarStreamProvider for ReqwestNarStreamProvider {
    async fn stream_nar(
        &self,
        locations: &[NarFileLocation],
        headers: &PassthroughHeaders,
    ) -> (
        AnyhowResult<Option<NarStreamData>>,
        Vec<NarStreamOpenAttempt>,
    ) {
        tracing::debug!(urls = ?locations.iter().map(|l| l.source_url()).collect::<Vec<_>>(), "opening nar file streams from substituters");

        if locations.is_empty() {
            return (Ok(None), Vec::new());
        }

        let mut set = JoinSet::new();
        for location in locations {
            let location = location.clone();
            let headers = headers.clone();

            let client = Arc::clone(&self.client);
            let credentials = Arc::clone(&self.credentials);
            let endpoint_managers = self.endpoint_managers.clone();

            set.spawn(async move {
                // Endpoint-eligible locations try each usable endpoint in
                // order; anything else uses the default client exactly once.
                let endpoint_plan =
                    endpoint_ips_for(location.substituter().url().host(), &endpoint_managers);
                let endpoint_manager = endpoint_plan.as_ref().map(|(manager, _)| Arc::clone(manager));
                let mut clients: Vec<(Option<IpAddr>, Arc<StreamingClient>)> = endpoint_plan
                    .map(|(manager, ips)| {
                        ips.into_iter()
                            .filter_map(|ip| {
                                manager
                                    .client_for(ip)
                                    .map(|clients| (Some(ip), clients.streaming))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                // System DNS is always the final route. This also makes the
                // documented fallback effective when every admitted direct
                // endpoint or SNI proxy fails while opening a NAR stream.
                clients.push((None, Arc::clone(&client)));

                let mut last_response = None;
                let mut endpoint_failed = false;
                for (ip, stream_client) in clients {
                    if let Some(ip) = ip {
                        tracing::trace!(url = %location.source_url(), %ip, "opening nar stream via endpoint");
                    } else if endpoint_failed {
                        tracing::warn!(url = %location.source_url(), "all endpoints failed; falling back to default client");
                    }
                    let request = stream_client.get(location.source_url().value()).configure({
                        let location = location.clone();
                        let headers = headers.clone();
                        let credentials = Arc::clone(&credentials);
                        move |request| {
                            let mut request = request.headers(headers.to_headers());

                            if let Some(credential) = credentials.lookup(location.source_url()) {
                                request = request
                                    .basic_auth(credential.login.clone(), credential.secret.clone());
                            }

                            request
                        }
                    });

                    let attempt = if let Some(timeout) = location.timeout() {
                        tokio::time::timeout(timeout, request.send()).await
                    } else {
                        Ok(request.send().await)
                    };

                    let retry_with_next_endpoint = match &attempt {
                        // Success and NotFound (the resource is absent, the
                        // endpoint is healthy) are final for this location.
                        Ok(Ok(_)) | Ok(Err(StreamHttpBodyError::NotFound)) => false,
                        Ok(Err(err)) => {
                            if let Some(ip) = ip {
                                let kind = match err {
                                    StreamHttpBodyError::Transport { source } => {
                                        classify_endpoint_failure(source)
                                    }
                                    _ => EndpointFailureKind::Transient,
                                };
                                tracing::debug!(url = %location.source_url(), %ip, ?kind, error = %err, "endpoint nar stream failed; trying next endpoint");
                                endpoint_failed = true;
                                endpoint_manager
                                    .as_ref()
                                    .expect("endpoint manager is present for endpoint attempts")
                                    .report_failure(ip, kind);
                            }
                            ip.is_some()
                        }
                        Err(_) => {
                            if let Some(ip) = ip {
                                tracing::debug!(url = %location.source_url(), %ip, "endpoint nar stream timed out; trying next endpoint");
                                endpoint_failed = true;
                                endpoint_manager
                                    .as_ref()
                                    .expect("endpoint manager is present for endpoint attempts")
                                    .report_failure(ip, EndpointFailureKind::Transient);
                            }
                            ip.is_some()
                        }
                    };

                    last_response = Some(attempt);
                    if !retry_with_next_endpoint {
                        break;
                    }
                }
                let response = last_response.expect("at least one stream attempt was made");
                (location.clone(), response)
            });
        }

        let mut not_found_count = 0;
        let mut attempts = Vec::new();

        while let Some(result) = set.join_next().await {
            let Ok((location, response)) = result else {
                continue;
            };
            let url = location.source_url();

            match response {
                Ok(Ok(response)) => {
                    attempts.push(NarStreamOpenAttempt::Successful {
                        source_url: url.clone(),
                    });
                    let response = Self::wrap_ok_response(url.clone(), response);
                    return (response, attempts);
                }
                Ok(Err(StreamHttpBodyError::NotFound)) => {
                    not_found_count += 1;
                    attempts.push(NarStreamOpenAttempt::Successful {
                        source_url: url.clone(),
                    });
                }
                Ok(Err(e)) => {
                    attempts.push(NarStreamOpenAttempt::ServiceError {
                        source_url: url.clone(),
                    });
                    tracing::debug!(%url, error = %e, "failed to request nar from substituter");
                }
                Err(_) => {
                    if let Some(timeout) = location.timeout() {
                        attempts.push(NarStreamOpenAttempt::Offline {
                            source_url: url.clone(),
                        });
                        tracing::debug!(%url, timeout_secs = %timeout.as_secs(), "timeout for requesting nar from substituter elapsed");
                    }
                }
            }
        }

        if not_found_count == locations.len() {
            (Ok(None), attempts)
        } else {
            let err = Err(anyhow::anyhow!("could not fetch nar from any substituter"));
            (err, attempts)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use reqwest::Client;
    use selector4nix_streaming::throttler::{PerHostHttpThrottler, ThrottlingOptions};

    use super::*;
    use crate::infrastructure::dns::doh_resolver::DohResolver;
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
            Arc::new(DohResolver::new()),
            user_candidates,
            false,
            Vec::new(),
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
}
