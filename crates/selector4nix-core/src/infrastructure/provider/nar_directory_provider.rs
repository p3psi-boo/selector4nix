use std::sync::Arc;

use anyhow::Result as AnyhowResult;
use async_trait::async_trait;
use http::{StatusCode, header};
use reqwest::{Client, RequestBuilder};
use tokio::task::JoinSet;

use crate::domain::common::passthrough_headers::PassthroughHeaders;
use crate::domain::common::url::Url;
use crate::domain::nar_info::model::StorePathHash;
use crate::domain::nar_info::port::{
    ListDirectoryAttempt, ListDirectoryData, NarDirectoryProvider,
};
use crate::domain::substituter::model::SubstituterMeta;
use crate::infrastructure::config::AppCredential;
use crate::infrastructure::endpoint::registry::EndpointManagerRegistry;
use crate::infrastructure::provider::nar_info_provider::{
    classify_endpoint_failure, endpoint_ips_for,
};

pub struct ReqwestNarDirectoryProvider {
    client: Client,
    credentials: Arc<AppCredential>,
    endpoint_managers: EndpointManagerRegistry,
}

impl ReqwestNarDirectoryProvider {
    pub fn new(
        client: Client,
        credentials: Arc<AppCredential>,
        endpoint_managers: EndpointManagerRegistry,
    ) -> Self {
        Self {
            client,
            credentials,
            endpoint_managers,
        }
    }
}

#[async_trait]
impl NarDirectoryProvider for ReqwestNarDirectoryProvider {
    async fn list(
        &self,
        substituters: &[SubstituterMeta],
        store_path_hash: &StorePathHash,
        headers: &PassthroughHeaders,
    ) -> (
        AnyhowResult<Option<ListDirectoryData>>,
        Vec<ListDirectoryAttempt>,
    ) {
        tracing::debug!(substituter_urls = ?substituters.iter().map(|s| s.url().to_string()).collect::<Vec<_>>(), hash = %store_path_hash.value(), "listing directory of store path from substituters");

        let mut pending = JoinSet::new();
        for substituter in substituters {
            let url = store_path_hash.on_substituter_listing(substituter);
            let headers = headers.clone();
            let client = self.client.clone();
            let credentials = Arc::clone(&self.credentials);
            let endpoint_managers = self.endpoint_managers.clone();
            let substituter_url = substituter.url().clone();
            pending.spawn(async move {
                if let Some((manager, ips)) = endpoint_ips_for(url.host(), &endpoint_managers) {
                    for ip in ips {
                        let Some(clients) = manager.client_for(ip) else {
                            continue;
                        };
                        let request = build_request(&clients.http, &url, &headers, &credentials);
                        match request.send().await {
                            Ok(response) => return handle_response(response, substituter_url).await,
                            Err(error) => {
                                let kind = classify_endpoint_failure(&error);
                                tracing::debug!(%url, %ip, ?kind, %error, "endpoint directory request failed; trying next endpoint");
                                manager.report_failure(ip, kind);
                            }
                        }
                    }
                    tracing::warn!(%url, "all endpoints failed; falling back to default client");
                }

                let request = build_request(&client, &url, &headers, &credentials);
                get_response(request, substituter_url).await
            });
        }

        let mut has_error = false;
        let mut attempts = Vec::new();
        while let Some(res) = pending.join_next().await {
            let Ok((res, attempt)) = res else {
                continue;
            };

            has_error |= res.is_err();
            let attempt = attempts.push_mut(attempt);
            if let Ok(Some(data)) = res {
                tracing::debug!(substituter_url = %attempt.substituter_url(), hash = %store_path_hash.value(), "fetched entry list in directory of store path");
                return (Ok(Some(data)), attempts);
            }
        }

        if !has_error {
            tracing::debug!(hash = %store_path_hash.value(), "tried listing non-existent directory of store path");
            (Ok(None), attempts)
        } else {
            tracing::debug!(hash = %store_path_hash.value(), "failed to list directory of store path");
            let err = Err(anyhow::anyhow!(
                "could not send list directory request for store path hash {}",
                store_path_hash.value()
            ));
            (err, attempts)
        }
    }
}

fn build_request(
    client: &Client,
    url: &Url,
    headers: &PassthroughHeaders,
    credentials: &AppCredential,
) -> RequestBuilder {
    let request = client.get(url.value()).headers(headers.to_headers());
    if let Some(credential) = credentials.lookup(url) {
        request.basic_auth(credential.login.clone(), credential.secret.clone())
    } else {
        request
    }
}

async fn get_response(
    request: RequestBuilder,
    substituter_url: Url,
) -> (Result<Option<ListDirectoryData>, ()>, ListDirectoryAttempt) {
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            if err.is_timeout() || err.is_connect() || err.is_request() {
                let attempt = ListDirectoryAttempt::Offline { substituter_url };
                return (Ok(None), attempt);
            } else {
                let attempt = ListDirectoryAttempt::ServiceError { substituter_url };
                return (Err(()), attempt);
            };
        }
    };

    handle_response(response, substituter_url).await
}

async fn handle_response(
    response: reqwest::Response,
    substituter_url: Url,
) -> (Result<Option<ListDirectoryData>, ()>, ListDirectoryAttempt) {
    match response.status() {
        StatusCode::OK => {
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok().map(ToOwned::to_owned));
            let content_encoding = response
                .headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|h| h.to_str().ok().map(ToOwned::to_owned));

            let Ok(content) = response.bytes().await else {
                let attempt = ListDirectoryAttempt::ServiceError { substituter_url };
                return (Err(()), attempt);
            };

            let data = ListDirectoryData {
                content,
                content_type,
                content_encoding,
            };
            let attempt = ListDirectoryAttempt::Successful { substituter_url };
            (Ok(Some(data)), attempt)
        }
        StatusCode::NOT_FOUND | StatusCode::FORBIDDEN => {
            let attempt = ListDirectoryAttempt::Successful { substituter_url };
            (Ok(None), attempt)
        }
        _ => {
            let attempt = ListDirectoryAttempt::ServiceError { substituter_url };
            (Err(()), attempt)
        }
    }
}
