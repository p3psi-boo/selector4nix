use std::net::IpAddr;
use std::sync::Arc;

use crate::AppError;
use crate::domain::common::url::Url;
use crate::domain::substituter::SubstituterRepository;
use crate::infrastructure::endpoint::registry::EndpointManagerRegistry;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AddSniProxyCommand {
    pub substituter_url: Url,
    pub ip: IpAddr,
}

pub struct AddSniProxyUseCase {
    substituter_repository: Arc<dyn SubstituterRepository>,
    endpoint_managers: EndpointManagerRegistry,
}

impl AddSniProxyUseCase {
    pub fn new(
        substituter_repository: Arc<dyn SubstituterRepository>,
        endpoint_managers: EndpointManagerRegistry,
    ) -> Self {
        Self {
            substituter_repository,
            endpoint_managers,
        }
    }

    pub async fn run(&self, command: AddSniProxyCommand) -> Result<(), AppError> {
        tracing::info!(url = %command.substituter_url, ip = %command.ip, "adding SNI proxy");

        if self
            .substituter_repository
            .get(&command.substituter_url)
            .await
            .is_none()
        {
            return Err(AppError::not_found("substituter does not exist"));
        }

        let Some(manager) = self
            .endpoint_managers
            .for_host(command.substituter_url.host())
        else {
            tracing::info!(url = %command.substituter_url, "rejected SNI proxy for unmanaged substituter");
            return Err(AppError::rule(
                "SNI proxy optimization is not enabled for this substituter",
            ));
        };

        if !manager.add_sni_proxy(command.ip).await {
            tracing::info!(url = %command.substituter_url, ip = %command.ip, "rejected duplicate SNI proxy");
            return Err(AppError::rule("SNI proxy already exists"));
        }

        tracing::info!(url = %command.substituter_url, ip = %command.ip, "added SNI proxy");
        Ok(())
    }
}
