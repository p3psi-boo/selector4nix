//! Registry routing a substituter host to its endpoint manager.

use std::sync::Arc;

use super::manager::EndpointManager;

/// Routes a substituter host to its endpoint manager, if any.
#[derive(Default, Clone)]
pub struct EndpointManagerRegistry {
    managers: Vec<Arc<EndpointManager>>,
}

impl EndpointManagerRegistry {
    pub fn new(managers: Vec<Arc<EndpointManager>>) -> Self {
        Self { managers }
    }

    pub fn is_empty(&self) -> bool {
        self.managers.is_empty()
    }

    /// Exact host match; `None` when no manager is bound to `host`.
    pub fn for_host(&self, host: &str) -> Option<Arc<EndpointManager>> {
        self.managers
            .iter()
            .find(|manager| manager.host() == host)
            .map(Arc::clone)
    }

    /// All registered managers, for the periodic refresh task to drive.
    pub fn managers(&self) -> &[Arc<EndpointManager>] {
        &self.managers
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
    use crate::domain::common::url::Url;
    use crate::infrastructure::dns::doh_resolver::DohResolver;
    use crate::infrastructure::provider::{EndpointClientPool, EndpointProbingProvider};

    fn make_manager(host: &str) -> Arc<EndpointManager> {
        let pool = Arc::new(EndpointClientPool::new(
            host.to_string(),
            443,
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
        Arc::new(EndpointManager::new(
            host.to_string(),
            Url::new(&format!("https://{host}")).unwrap(),
            pool,
            probing,
            Arc::new(DohResolver::new()),
            Vec::<IpAddr>::new(),
            false,
            Vec::new(),
        ))
    }

    #[test]
    fn for_host_matches_exactly() {
        let registry = EndpointManagerRegistry::new(vec![
            make_manager("cache.nixos.org"),
            make_manager("foo.cachix.org"),
        ]);

        let manager = registry.for_host("foo.cachix.org").unwrap();
        assert_eq!(manager.host(), "foo.cachix.org");
    }

    #[test]
    fn for_host_returns_none_on_miss() {
        let registry = EndpointManagerRegistry::new(vec![make_manager("cache.nixos.org")]);

        assert!(registry.for_host("bar.cachix.org").is_none());
        assert!(registry.for_host("nixos.org").is_none());
    }

    #[test]
    fn empty_registry_matches_nothing() {
        let registry = EndpointManagerRegistry::default();

        assert!(registry.is_empty());
        assert!(registry.for_host("cache.nixos.org").is_none());
        assert!(registry.managers().is_empty());
    }
}
