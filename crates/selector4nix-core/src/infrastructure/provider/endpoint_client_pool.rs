//! Pool of endpoint-bound HTTP clients keyed by endpoint IP.
//!
//! Each endpoint client pins the TCP connection target of a logical host
//! (e.g. `cache.nixos.org`) to a specific IP address, equivalent to
//! `curl --resolve`. All clients in the pool share a single
//! [`PerHostHttpThrottler`] so that per-host concurrency limits are enforced
//! across endpoints of the same logical host.

use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use reqwest::{Client, ClientBuilder};
use selector4nix_streaming::StreamingClient;
use selector4nix_streaming::throttler::PerHostHttpThrottler;

/// HTTP clients bound to a single endpoint IP.
pub struct EndpointClientSet {
    pub http: Client,
    pub streaming: Arc<StreamingClient>,
}

struct PoolEntry {
    clients: EndpointClientSet,
    last_used: Instant,
}

/// Bounded pool of endpoint-bound clients with least-recently-used eviction.
pub struct EndpointClientPool {
    host: String,
    port: u16,
    factory: Arc<dyn Fn() -> ClientBuilder + Send + Sync>,
    throttler: Arc<PerHostHttpThrottler>,
    enable_chunked_streaming: bool,
    chunk_max_len: NonZeroUsize,
    window_max_len: NonZeroUsize,
    capacity: usize,
    entries: DashMap<IpAddr, PoolEntry>,
}

impl EndpointClientPool {
    // The streaming parameters mirror `StreamingClient::new`; grouping them would only obscure
    // the one-to-one forwarding.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        host: String,
        port: u16,
        factory: Arc<dyn Fn() -> ClientBuilder + Send + Sync>,
        throttler: Arc<PerHostHttpThrottler>,
        enable_chunked_streaming: bool,
        chunk_max_len: NonZeroUsize,
        window_max_len: NonZeroUsize,
        capacity: usize,
    ) -> Self {
        Self {
            host,
            port,
            factory,
            throttler,
            enable_chunked_streaming,
            chunk_max_len,
            window_max_len,
            capacity,
            entries: DashMap::new(),
        }
    }

    pub fn get_or_build(&self, ip: IpAddr) -> EndpointClientSet {
        if let Some(mut entry) = self.entries.get_mut(&ip) {
            entry.last_used = Instant::now();
            let PoolEntry { clients, .. } = &*entry;
            return EndpointClientSet {
                http: clients.http.clone(),
                streaming: Arc::clone(&clients.streaming),
            };
        }

        self.evict_if_full();

        let clients = self.build(ip);
        self.entries.insert(
            ip,
            PoolEntry {
                clients: EndpointClientSet {
                    http: clients.http.clone(),
                    streaming: Arc::clone(&clients.streaming),
                },
                last_used: Instant::now(),
            },
        );
        clients
    }

    fn build(&self, ip: IpAddr) -> EndpointClientSet {
        let endpoint_addrs = [SocketAddr::new(ip, self.port)];

        let http = (self.factory)()
            .resolve_to_addrs(self.host.as_str(), &endpoint_addrs)
            .no_proxy()
            .build()
            .expect("invalid reqwest client configuration");

        let streaming_builder = (self.factory)()
            .resolve_to_addrs(self.host.as_str(), &endpoint_addrs)
            .no_proxy();
        let streaming = Arc::new(StreamingClient::with_shared_throttler(
            streaming_builder,
            Arc::clone(&self.throttler),
            self.enable_chunked_streaming,
            self.chunk_max_len,
            self.window_max_len,
        ));

        EndpointClientSet { http, streaming }
    }

    fn evict_if_full(&self) {
        while self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|entry| entry.last_used)
                .map(|entry| *entry.key());
            match oldest {
                Some(ip) => {
                    self.entries.remove(&ip);
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selector4nix_streaming::throttler::ThrottlingOptions;

    fn pool(capacity: usize) -> EndpointClientPool {
        EndpointClientPool::new(
            "cache.nixos.org".to_string(),
            443,
            Arc::new(Client::builder),
            Arc::new(PerHostHttpThrottler::new(ThrottlingOptions::new(
                NonZeroUsize::new(8).unwrap(),
            ))),
            false,
            NonZeroUsize::new(1024).unwrap(),
            NonZeroUsize::new(4096).unwrap(),
            capacity,
        )
    }

    #[test]
    fn get_or_build_caches_clients_per_ip() {
        let pool = pool(16);
        let ip: IpAddr = "151.101.1.91".parse().unwrap();

        let first = pool.get_or_build(ip);
        let second = pool.get_or_build(ip);

        assert!(Arc::ptr_eq(&first.streaming, &second.streaming));
        assert_eq!(pool.entries.len(), 1);
    }

    #[test]
    fn get_or_build_caches_distinct_ips_separately() {
        let pool = pool(16);
        let a: IpAddr = "151.101.1.91".parse().unwrap();
        let b: IpAddr = "151.101.65.91".parse().unwrap();

        let clients_a = pool.get_or_build(a);
        let clients_b = pool.get_or_build(b);

        assert!(!Arc::ptr_eq(&clients_a.streaming, &clients_b.streaming));
        assert_eq!(pool.entries.len(), 2);
    }

    #[test]
    fn evicts_least_recently_used_entry_when_full() {
        let pool = pool(3);
        let ip1: IpAddr = "151.101.1.91".parse().unwrap();
        let ip2: IpAddr = "151.101.65.91".parse().unwrap();
        let ip3: IpAddr = "151.101.129.91".parse().unwrap();
        let ip4: IpAddr = "151.101.193.91".parse().unwrap();

        pool.get_or_build(ip1);
        std::thread::sleep(std::time::Duration::from_millis(10));
        pool.get_or_build(ip2);
        std::thread::sleep(std::time::Duration::from_millis(10));
        pool.get_or_build(ip3);

        // Touch ip1 so ip2 becomes the least recently used entry.
        pool.get_or_build(ip1);
        std::thread::sleep(std::time::Duration::from_millis(10));

        pool.get_or_build(ip4);

        assert_eq!(pool.entries.len(), 3);
        assert!(pool.entries.contains_key(&ip1));
        assert!(!pool.entries.contains_key(&ip2));
        assert!(pool.entries.contains_key(&ip3));
        assert!(pool.entries.contains_key(&ip4));
    }
}
