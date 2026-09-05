use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::application::actor::substituter::{SubstituterActorRegistry, SubstituterRequest};
use crate::domain::common::url::Url;
use crate::domain::substituter::SubstituterRepository;
use crate::{AppError, AppResultExt};

pub struct DisableSubstituterUseCase {
    substituter_repository: Arc<dyn SubstituterRepository>,
    substituter_registry: Arc<SubstituterActorRegistry>,
    mutation_lock: Semaphore,
}

impl DisableSubstituterUseCase {
    pub fn new(
        substituter_repository: Arc<dyn SubstituterRepository>,
        substituter_registry: Arc<SubstituterActorRegistry>,
    ) -> Self {
        Self {
            substituter_repository,
            substituter_registry,
            mutation_lock: Semaphore::new(1),
        }
    }

    pub async fn run(&self, url: Url) -> Result<(), AppError> {
        tracing::info!(%url, "disabling substituter");

        let _permit = self
            .mutation_lock
            .acquire()
            .await
            .throw_catastrophic("substituter mutation lock closed")?;

        let Some(substituter) = self.substituter_repository.get(&url).await else {
            return Err(AppError::not_found("substituter does not exist"));
        };
        if !substituter.is_enabled() {
            tracing::info!(%url, "substituter already disabled");
            return Ok(());
        }

        let enabled_count = self
            .substituter_repository
            .query_all()
            .await
            .iter()
            .filter(|s| s.is_enabled())
            .count();
        if enabled_count <= 1 {
            tracing::info!(%url, "rejected disabling last enabled substituter");
            return Err(AppError::rule(
                "at least one substituter must remain enabled",
            ));
        }

        let address = self.substituter_registry.get(&url).await;
        address
            .ask(|reply_to| SubstituterRequest::Disable { reply_to })
            .await
            .throw_catastrophic("`SubstituterActor` terminated unexpectedly")?;

        tracing::info!(%url, "disabled substituter");
        Ok(())
    }
}
