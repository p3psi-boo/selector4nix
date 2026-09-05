use std::sync::Arc;

use crate::AppError;
use crate::application::actor::substituter::SubstituterActorRegistry;
use crate::domain::common::url::Url;
use crate::domain::substituter::SubstituterRepository;
use crate::domain::substituter::model::{Availability, Priority, Substituter, SubstituterMeta};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AddSubstituterCommand {
    pub url: Url,
    pub priority: Priority,
    pub storage_url: Option<Url>,
}

pub struct AddSubstituterUseCase {
    substituter_repository: Arc<dyn SubstituterRepository>,
    substituter_registry: Arc<SubstituterActorRegistry>,
}

impl AddSubstituterUseCase {
    pub fn new(
        substituter_repository: Arc<dyn SubstituterRepository>,
        substituter_registry: Arc<SubstituterActorRegistry>,
    ) -> Self {
        Self {
            substituter_repository,
            substituter_registry,
        }
    }

    pub async fn run(&self, command: AddSubstituterCommand) -> Result<(), AppError> {
        tracing::info!(url = %command.url, priority = %command.priority.value(), "adding substituter");

        let mut meta = SubstituterMeta::new(command.url.clone(), command.priority);
        if let Some(storage_url) = command.storage_url {
            meta = meta.with_storage_url(storage_url);
        }
        let substituter = Substituter::new(meta, Availability::Normal);

        if !self.substituter_repository.create(substituter).await {
            tracing::info!(url = %command.url, "rejected duplicate substituter");
            return Err(AppError::rule("substituter already exists"));
        }

        let _ = self.substituter_registry.get(&command.url).await;
        tracing::info!(url = %command.url, "added substituter");
        Ok(())
    }
}
