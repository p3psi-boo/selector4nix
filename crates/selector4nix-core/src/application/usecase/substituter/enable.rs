use std::sync::Arc;

use crate::application::actor::substituter::{SubstituterActorRegistry, SubstituterRequest};
use crate::domain::common::url::Url;
use crate::domain::substituter::SubstituterRepository;
use crate::{AppError, AppResultExt};

pub struct EnableSubstituterUseCase {
    substituter_repository: Arc<dyn SubstituterRepository>,
    substituter_registry: Arc<SubstituterActorRegistry>,
}

impl EnableSubstituterUseCase {
    pub fn new(
        substituter_repository: Arc<dyn SubstituterRepository>,
        substituter_registry: Arc<SubstituterActorRegistry>,
    ) -> Self {
        Self {
            substituter_repository,
            substituter_registry,
        }
    }

    pub async fn run(&self, url: Url) -> Result<(), AppError> {
        tracing::info!(%url, "enabling substituter");

        let Some(substituter) = self.substituter_repository.get(&url).await else {
            return Err(AppError::not_found("substituter does not exist"));
        };
        if substituter.is_enabled() {
            tracing::info!(%url, "substituter already enabled");
            return Ok(());
        }

        let address = self.substituter_registry.get(&url).await;
        address
            .ask(|reply_to| SubstituterRequest::Enable { reply_to })
            .await
            .throw_catastrophic("`SubstituterActor` terminated unexpectedly")?;

        tracing::info!(%url, "enabled substituter");
        Ok(())
    }
}
