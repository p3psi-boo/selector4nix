use std::sync::Arc;

use async_trait::async_trait;
use selector4nix_actor::registry::{AsyncFactory, RegistryBuilder};
use selector4nix_core::AppErrorKind;
use selector4nix_core::application::actor::substituter::SubstituterActor;
use selector4nix_core::application::usecase::substituter::{
    AddSubstituterCommand, AddSubstituterUseCase, DisableSubstituterUseCase,
    EnableSubstituterUseCase,
};
use selector4nix_core::domain::common::url::Url;
use selector4nix_core::domain::substituter::SubstituterRepository;
use selector4nix_core::domain::substituter::SubstituterService;
use selector4nix_core::domain::substituter::model::test_support::make_substituter_normal_with_url;
use selector4nix_core::domain::substituter::model::{
    PeriodicProbingOption, Priority, SubstituterMeta,
};
use selector4nix_core::domain::substituter::port::{
    ProbeSubstituterError, SubstituterProbingProvider,
};
use selector4nix_core::infrastructure::repository::InMemorySubstituterRepository;

struct NoopProbingProvider;

#[async_trait]
impl SubstituterProbingProvider for NoopProbingProvider {
    async fn probe_substituter(
        &self,
        _substituter: &SubstituterMeta,
    ) -> Result<(), ProbeSubstituterError> {
        Ok(())
    }
}

struct TestStack {
    repository: Arc<InMemorySubstituterRepository>,
    add: AddSubstituterUseCase,
    enable: EnableSubstituterUseCase,
    disable: DisableSubstituterUseCase,
}

fn setup() -> TestStack {
    let repository = Arc::new(InMemorySubstituterRepository::new());
    let service = Arc::new(SubstituterService::new(PeriodicProbingOption::None));
    let probing: Arc<dyn SubstituterProbingProvider> = Arc::new(NoopProbingProvider);
    let registry = Arc::new(
        RegistryBuilder::new()
            .factory(AsyncFactory::new({
                let service = service.clone();
                let probing = probing.clone();
                let repository = repository.clone();
                move |url: &Url| {
                    let service = service.clone();
                    let probing = probing.clone();
                    let repository = repository.clone();
                    let url = url.clone();
                    async move {
                        let init = repository.get(&url).await;
                        SubstituterActor::new(init, service, probing, repository).run()
                    }
                }
            }))
            .build(),
    );

    TestStack {
        repository: repository.clone(),
        add: AddSubstituterUseCase::new(repository.clone(), registry.clone()),
        enable: EnableSubstituterUseCase::new(repository.clone(), registry.clone()),
        disable: DisableSubstituterUseCase::new(repository, registry),
    }
}

fn url(value: &str) -> Url {
    Url::new(value).unwrap()
}

fn priority(value: u32) -> Priority {
    Priority::new(value).unwrap()
}

#[tokio::test]
async fn add_inserts_selectable_substituter() {
    let stack = setup();
    let added = url("https://mirror.example.com/");

    stack
        .add
        .run(AddSubstituterCommand {
            url: added.clone(),
            priority: priority(10),
            storage_url: None,
        })
        .await
        .unwrap();

    let stored = stack.repository.get(&added).await.unwrap();
    assert!(stored.is_enabled());
    assert!(stored.is_selectable());
    assert_eq!(stored.priority(), priority(10));
    assert_eq!(stack.repository.query_all_available().await.len(), 1);
}

#[tokio::test]
async fn add_uses_custom_storage_url() {
    let stack = setup();
    let added = url("https://mirror.example.com/");
    let storage = url("https://cdn.example.com/nar/");

    stack
        .add
        .run(AddSubstituterCommand {
            url: added.clone(),
            priority: priority(40),
            storage_url: Some(storage.clone()),
        })
        .await
        .unwrap();

    let stored = stack.repository.get(&added).await.unwrap();
    assert_eq!(stored.target().storage_url(), &storage);
}

#[tokio::test]
async fn add_rejects_duplicate_url() {
    let stack = setup();
    let added = url("https://mirror.example.com/");
    let command = AddSubstituterCommand {
        url: added,
        priority: priority(40),
        storage_url: None,
    };

    stack.add.run(command.clone()).await.unwrap();
    let err = stack.add.run(command).await.unwrap_err();

    assert_eq!(err.kind(), AppErrorKind::Rule);
    assert!(err.to_string().contains("already exists"));
}

#[tokio::test]
async fn disable_removes_substituter_from_available_set() {
    let stack = setup();
    let kept = url("https://a.example.com/");
    let disabled = url("https://b.example.com/");
    stack
        .repository
        .save(make_substituter_normal_with_url(&kept))
        .await;
    stack
        .repository
        .save(make_substituter_normal_with_url(&disabled))
        .await;

    stack.disable.run(disabled.clone()).await.unwrap();

    let stored = stack.repository.get(&disabled).await.unwrap();
    assert!(!stored.is_enabled());
    assert!(!stored.is_selectable());
    assert!(!stack.repository.exists_available(&disabled).await);
    assert!(stack.repository.exists_available(&kept).await);
}

#[tokio::test]
async fn disable_rejects_last_enabled_substituter() {
    let stack = setup();
    let only = url("https://a.example.com/");
    stack
        .repository
        .save(make_substituter_normal_with_url(&only))
        .await;

    let err = stack.disable.run(only.clone()).await.unwrap_err();

    assert_eq!(err.kind(), AppErrorKind::Rule);
    assert!(stack.repository.exists_available(&only).await);
}

#[tokio::test]
async fn enable_restores_disabled_substituter() {
    let stack = setup();
    let kept = url("https://a.example.com/");
    let toggled = url("https://b.example.com/");
    stack
        .repository
        .save(make_substituter_normal_with_url(&kept))
        .await;
    stack
        .repository
        .save(make_substituter_normal_with_url(&toggled))
        .await;
    stack.disable.run(toggled.clone()).await.unwrap();

    stack.enable.run(toggled.clone()).await.unwrap();

    let stored = stack.repository.get(&toggled).await.unwrap();
    assert!(stored.is_enabled());
    assert!(stored.is_selectable());
    assert!(stack.repository.exists_available(&toggled).await);
}

#[tokio::test]
async fn enable_is_idempotent_for_already_enabled_substituter() {
    let stack = setup();
    let only = url("https://a.example.com/");
    stack
        .repository
        .save(make_substituter_normal_with_url(&only))
        .await;

    stack.enable.run(only.clone()).await.unwrap();

    assert!(stack.repository.exists_available(&only).await);
}

#[tokio::test]
async fn enable_rejects_unknown_url() {
    let stack = setup();
    let err = stack
        .enable
        .run(url("https://missing.example.com/"))
        .await
        .unwrap_err();

    assert_eq!(err.kind(), AppErrorKind::NotFound);
}
