# Configuration Reference

`selector4nix` reads a TOML configuration file from the first of these locations:

1. The path specified by the `--config-file` command line argument
2. The path specified by the `SELECTOR4NIX_CONFIG_FILE` environment variable
3. `./selector4nix.toml` in the current directory
4. `/etc/selector4nix/selector4nix.toml`

## `server`

Server listen address.

### `server.ip`

- Type: IP Address

The IP address that `selector4nix` listens on.

### `server.port`

- Type: Port
- Default: `5496`

The port that `selector4nix` listens on.

## `network`

Network request settings.

### `network.nar_info_timeout_secs`

- Type: Positive Integer
- Default: `30`

Timeout in seconds for NAR info lookup requests.

### `network.nar_timeout_secs`

- Type: Positive Integer
- Default: `30`

Timeout in seconds for NAR file downloads, also used as connect timeout.

### `network.max_concurrent_requests`

- Type: Positive Integer
- Default: `12`

Maximum number of concurrent outgoing NAR file streaming requests, applied per distinct substituter host. The overall ceiling across the proxy is `max_concurrent_requests` multiplied by the number of distinct substituter hosts. Individual hosts can deviate from this default via `substituters[].max_concurrent_requests`.

### `network.chunked_streaming`

- Type: Boolean
- Default: `true`

When enabled, NAR files are downloaded using concurrent multi-connection chunked transfer if the upstream substituter supports Range requests. When multiple NAR files are downloaded concurrently, available connections are shared fairly across files so that no single file monopolizes the bandwidth.

### `network.streaming_chunk_max_len`

- Type: Positive Integer
- Default: `4194304`

Maximum size in bytes of each chunk when downloading NAR files. The default value is 4 MiB.

### `network.streaming_window_max_len`

- Type: Positive Integer
- Default: `8`

Maximum number of chunks that may be in flight simultaneously for a single NAR file. The effective concurrency is also bounded by `max_concurrent_requests`. When the per-host concurrency limit is saturated, new chunks defer in favor of streaming other NAR files.

### `network.tolerance_msecs`

- Type: Natural
- Default: `50`

Latency tolerance window in milliseconds. The preference of a substituter is calculated as `-tolerance * priority - latency`. After the fastest substituter responds, other substituters have additional milliseconds equal to the difference between their preference and the current best before being pruned. Only effective under the `preference` resolution policy (see `proxy.resolution_policy`).

### `network.ignore_nar_info_error`

- Type: Boolean
- Default: `false`

When enabled, NAR info lookup errors from substituters are treated as not-found instead of infrastructure errors.

> **Warning:** This may cause incorrect judgments about whether a NAR info actually exists. A substituter returning an error will be interpreted as "not found", which may not be the case.

### `network.periodic_probing`

- Type: Boolean
- Default: `true`

When enabled, `selector4nix` continuously probes substituters every 30 seconds to detect failures early. Probing during retry recovery always occurs regardless of this setting.

## `fastly_optimization`

Fastly endpoint optimization for the official Nix cache. This is a global section and does not belong to any single substituter.

When enabled, requests to the substituter whose host is `cache.nixos.org` are sent to Fastly edge endpoints discovered and probed at runtime, instead of relying on system DNS resolution. Only the TCP connection target IP is overridden (equivalent to `curl --resolve`): the request URL, the HTTP `Host` header, and the TLS SNI all remain `cache.nixos.org`, and certificate verification is performed strictly as usual. Other substituters are unaffected.

Candidate endpoints come from three sources: DNS-over-HTTPS lookups against `cloudflare-dns.com` and `dns.google` (bypassing the system DNS, so fake-ip environments work too), IP literals configured via `candidates`, and region-derived candidates when `derive_regions` is enabled. Every candidate must pass an admission probe — a TLS handshake plus a `GET /nix-cache-info` request returning 200 — before it is considered usable. Usable endpoints are ordered by their admission latency, and a failing endpoint automatically fails over to another endpoint of the same substituter. If no endpoint is usable, requests fall back to the default path using system DNS, behaving exactly as if this feature were disabled.

Note that endpoint requests do not use HTTP(S)_PROXY or other environment proxies; they connect directly to the selected IP. Under transparent proxies (TUN/fake-ip), all endpoints go through the same proxy chain, so endpoint preference is of limited benefit there — DoH-based discovery still works in that case. All Range requests of a chunked NAR download are pinned to the same endpoint, and the concurrency quota is still shared per logical host.

Enabling this section requires a substituter with host `cache.nixos.org` in the configuration; otherwise the server refuses to start.

`fastly_optimization.enabled` and `cloudflare_cache_proxy.enabled` are mutually exclusive: they are two alternative acceleration paths for `cache.nixos.org`.

### `fastly_optimization.enabled`

- Type: Boolean
- Default: `false`

Whether Fastly endpoint optimization is enabled.

### `fastly_optimization.candidates`

- Type: Array of IP Address
- Default: `[]`

Additional seed endpoint candidates as IP literals. Domain names are not accepted.

### `fastly_optimization.derive_regions`

- Type: Boolean
- Default: `false`

Whether to derive additional regional endpoint candidates from the discovered Fastly addresses (see [mosdns discussion #511](https://github.com/IrineSistiana/mosdns/discussions/511)).

## `cloudflare_cache_proxy`

Cloudflare reverse-proxy routing for the official Nix cache. This is a global section and does not belong to any single substituter.

When enabled, every configured substituter whose host is `cache.nixos.org` is routed through the configured proxy origin. The proxy URL is constructed as `PROXY_URL/{scheme}/{host}/{path}`. For example, `https://cache.nixos.org/nar/example.nar.xz` becomes `https://YOUR-CLOUDFLARE-PROXY.example/https/cache.nixos.org/nar/example.nar.xz` when `url = "https://YOUR-CLOUDFLARE-PROXY.example/"`. NAR info, NAR files, health probes, and directory listing requests all use the same route.

The original `https://cache.nixos.org/` entry remains in `[[substituters]]`; the rewrite happens during configuration loading. The section requires at least one such substituter. Its `url` must be an HTTPS origin without a query string or fragment.

To select a preferred Cloudflare edge IP rather than relying on the proxy host's normal DNS result, also set `cloudflare_optimization.enabled = true`. This creates an endpoint manager for the reverse-proxy host and reuses the configured `cloudflare_optimization.candidates`, `cloudflare_optimization.discovery_domains`, and `cloudflare_optimization.external_ip_lists` sources. Every candidate is admission-probed against the routed `nix-cache-info` endpoint before use.

This option and `fastly_optimization.enabled` are mutually exclusive. Set exactly one of them to `true` to select the acceleration path for `cache.nixos.org`; both may be `false` to use the normal direct cache route.

### `cloudflare_cache_proxy.enabled`

- Type: Boolean
- Default: `false`

Whether to route `cache.nixos.org` requests through `cloudflare_cache_proxy.url`.

### `cloudflare_cache_proxy.url`

- Type: HTTPS URL
- Default: unset

Origin URL of the Cloudflare-hosted reverse proxy. Required when `enabled` is `true`. The value may include a path prefix, but must not include a query string or fragment.

### `cloudflare_cache_proxy.bandwidth_probe`

An active, bounded Range-download benchmark for each newly admitted endpoint and for endpoints whose measurement is older than `refresh_secs`. It is only used for the Cloudflare cache-proxy host; Cachix substituters retain latency-only endpoint selection.

The default sample is the immutable `nar/1as5cn000kck2y35awm3825qvlvcnq08jbilwdlzdlv6pbidk3i4.nar.zst` object in `cache.nixos.org`, whose compressed size is larger than 10 MiB. The benchmark requests exactly the configured initial range with `Accept-Encoding: identity`, requires HTTP `206 Partial Content`, discards the received bytes, and never caches NAR content. It records TTFB and throughput; endpoints with a successful measurement are selected by their estimated 10 MiB download time, and unmeasured endpoints fall back to admission latency.

At most two endpoint benchmarks run concurrently by default, and the default interval is six hours. A failed bandwidth benchmark leaves an endpoint admitted; only the existing TLS and `nix-cache-info` admission result controls usability.

#### `cloudflare_cache_proxy.bandwidth_probe.enabled`

- Type: Boolean
- Default: `true`

Whether active bandwidth benchmarking is enabled.

#### `cloudflare_cache_proxy.bandwidth_probe.nar_path`

- Type: Relative `nar/` Path
- Default: `nar/1as5cn000kck2y35awm3825qvlvcnq08jbilwdlzdlv6pbidk3i4.nar.zst`

The sample NAR path. It must start with `nar/` and may not contain `..`, a query string, or a fragment.

#### `cloudflare_cache_proxy.bandwidth_probe.bytes`

- Type: Positive Integer
- Default: `10485760`

Number of bytes downloaded for each benchmark. The default is 10 MiB.

#### `cloudflare_cache_proxy.bandwidth_probe.refresh_secs`

- Type: Positive Integer
- Default: `21600`

Minimum age before an endpoint is benchmarked again.

#### `cloudflare_cache_proxy.bandwidth_probe.max_concurrent_probes`

- Type: Positive Integer
- Default: `2`

Maximum concurrent benchmark downloads for one Cloudflare cache-proxy host.

## `cloudflare_optimization`

Cloudflare endpoint optimization for Cachix substituters and the optional Cloudflare cache reverse proxy. This is a global section and does not belong to any single substituter.

When enabled, requests to substituters whose host is `cachix.org` or `*.cachix.org`, plus the configured `cloudflare_cache_proxy` host when that proxy is enabled, are sent to Cloudflare edge endpoints discovered and probed at runtime instead of relying on system DNS resolution. Unlike Fastly, any Cloudflare edge IP serves the correct certificate by SNI (verified in practice), so faster Cloudflare IPs can be discovered via third-party optimized-IP domains. The transport semantics are identical to `fastly_optimization`: only the TCP connection target IP is overridden (equivalent to `curl --resolve`); the request URL, the HTTP `Host` header, and the TLS SNI all remain the substituter's own host, and certificate verification is performed strictly as usual. Other substituters are unaffected.

Candidate endpoints come from four sources: DNS-over-HTTPS lookups of the substituter's own domain, DNS-over-HTTPS lookups of each domain in `discovery_domains`, plain-text endpoint lists in `external_ip_lists`, and IP literals configured via `candidates`. There is no region derivation (that is Fastly-specific). As with `fastly_optimization`, every candidate — regardless of source — must pass an admission probe (a TLS handshake plus a `GET /nix-cache-info` request returning 200) before it is considered usable. For a Cloudflare cache proxy with active benchmarking enabled, measured endpoints are then ordered by estimated download time; otherwise they are ordered by admission latency. A failing endpoint automatically fails over to another endpoint of the same substituter, and if no endpoint is usable, requests fall back to the default path using system DNS.

The default `discovery_domains` entry, `cloudflare.182682.xyz`, is a third-party-maintained list of optimized Cloudflare IPs. It is not operated by this project and may change or become unavailable at any time; because every candidate must pass the admission probe anyway, a stale or dead discovery domain merely yields fewer candidates and never affects correctness.

Endpoint requests do not use HTTP(S)_PROXY or other environment proxies; they connect directly to the selected IP. All Range requests of a chunked NAR download are pinned to the same endpoint, and the concurrency quota is still shared per logical host — same as `fastly_optimization`. Endpoint status is shown on the dashboard overview page.

Enabling this section requires at least one substituter with host `cachix.org` or `*.cachix.org`, or an enabled `cloudflare_cache_proxy`; otherwise the server refuses to start.

### `cloudflare_optimization.enabled`

- Type: Boolean
- Default: `false`

Whether Cloudflare endpoint optimization is enabled.

### `cloudflare_optimization.candidates`

- Type: Array of IP Address
- Default: `[]`

Additional seed endpoint candidates as IP literals. Domain names are not accepted.

### `cloudflare_optimization.discovery_domains`

- Type: Array of String
- Default: `["cloudflare.182682.xyz"]`

Third-party domains whose DNS-over-HTTPS answers are used as additional Cloudflare endpoint candidates. Set to `[]` explicitly to disable this source.

### `cloudflare_optimization.external_ip_lists`

- Type: Array of Tables
- Default: `[]`

HTTPS URLs for externally maintained Cloudflare preferred-IP lists. Each response is parsed as UTF-8 plain text: one IPv4 or IPv6 address per line, with blank lines and text after `#` ignored. Domains, CIDRs, and `IP:port` entries are ignored. Results are deduplicated with DoH and configured candidates. The provider caches the most recent successful result in memory; if a refresh fails, that stale result remains usable but every IP is still admission-probed.

#### `cloudflare_optimization.external_ip_lists[].url`

- Type: HTTPS URL

Location of the plain-text IP list.

#### `cloudflare_optimization.external_ip_lists[].refresh_secs`

- Type: Positive Integer
- Default: `3600`

How long the in-memory result is reused before fetching the list again. List refreshes are evaluated during the endpoint manager's approximately 30-minute refresh cycle, so shorter values take effect on that next cycle.

## `proxy`

Proxy behavior settings.

### `proxy.rewrite_nar_url`

- Type: Boolean
- Default: `true`

When enabled, the `URL` field in NAR info responses is rewritten according to `rewrite_to_target`. When disabled, the original full URL or relative path from the upstream substituter is preserved as-is and `rewrite_to_target` is ignored.

### `proxy.rewrite_to_target`

- Type: String of `"self"` or `"upstream"`
- Default: `"self"`

Controls how the `URL` field is rewritten when `rewrite_nar_url` is enabled. Only effective when `rewrite_nar_url = true`.

- `"self"`: Rewrite to a relative path (e.g. `URL: nar/<hash>.nar.xz`) so that NAR file requests go through `selector4nix`. This allows transparent fallback to other substituters when the original one becomes unavailable.
- `"upstream"`: Rewrite to the winning upstream substituter's storage URL (e.g. `URL: https://cache.nixos.org/nar/<hash>.nar.xz`). This normalizes URLs to a consistent upstream address rather than preserving whatever format each substituter returns. NAR file requests will go directly to the upstream substituter, bypassing `selector4nix`.

Note that the `URL` field in NAR info is opaque and varies across substituters: a given store path may map to different NAR URLs on different substituters, so fallback is not guaranteed to succeed when the NAR files are not identical across substituters.

### `proxy.resolution_policy`

- Type: String of `"preference"` or `"tier"`
- Default: `"preference"`

Controls how substituters are queried to resolve a NAR info.

- `"preference"`: All substituters are queried at once and the winner is picked by preference (`-tolerance * priority - latency`, governed by `network.tolerance_msecs`).
- `"tier"`: Substituters are queried tier by tier, from the highest priority (the lowest priority value) to the lowest. Substituters sharing the same `priority` value still race against each other, but a lower-priority tier is only queried when every higher-priority tier responded with not-found or an error. This keeps traffic on preferred mirrors (e.g. region-local ones), at the cost of serial lookup latency when higher-priority tiers lack the store path.

## `cache_info`

Cache info exposed via `/nix-cache-info` endpoint.

### `cache_info.store_dir`

- Type: String
- Default: `"/nix/store"`

Nix store directory path. Must be an absolute path.

### `cache_info.want_mass_query`

- Type: Boolean
- Default: `true`

Whether to advertise support for mass queries.

### `cache_info.priority`

- Type: Positive Integer
- Default: `40`

Substituter priority advertised to Nix clients.

## `cache`

Cache settings for NAR info content and NAR file location data.

There are two kinds of cache in this server: "cache" and "store". The former kind of caches are used to speed up accessing to entries, while the latter ones are for long-period storage, although "stores" are in-memory by default and you need to explicitly set a disk directory to enable persistence. These terms may seem confusing at first but it is how the server implements the caching mechanism in reality.

NAR info cache/store contains the NAR info content for each store path hash. NAR file cache/store keeps the location index mapping NAR file names to their source substituter, which is used when the server proxies NAR file download requests.

### `cache.nar_info_cache_capacity`

- Type: Positive Integer
- Default: `4096`

Maximum number of cached NAR info entries in the NAR info cache. This has no effect on the capacity of the NAR info store.

### `cache.nar_info_ttl_secs`

- Type: Positive Integer
- Default: `14400`

Time-to-live in seconds for cached NAR info entries.

### `cache.nar_file_cache_capacity`

- Type: Positive Integer
- Default: `4096`

Maximum number of cached NAR file location entries. This has no effect on the capacity of the NAR file store.

### `cache.nar_file_ttl_secs`

- Type: Positive Integer
- Default: `14400`

Time-to-live in seconds for cached NAR file location entries.

## `substituters`

Upstream substituter list. At least one entry is required.

### `substituters[].url`

- Type: URL

Base URL of the upstream substituter.

### `substituters[].storage_url`

- Type: URL
- Default: `"{substituters[].url}/nar/""`

Override the base URL used for NAR file downloads.

### `substituters[].priority`

- Type: Positive Integer
- Default: `40`

Priority of this substituter. Higher values mean lower priority.

### `substituters[].nar_info_timeout_secs`

- Type: Positive Integer | None
- Default: none

Per-substituter override for NAR info lookup timeout in seconds. When unset, falls back to `network.nar_info_timeout_secs`.

### `substituters[].nar_timeout_secs`

- Type: Positive Integer | None
- Default: none

Per-substituter override for NAR file download timeout in seconds. When unset, falls back to `network.nar_timeout_secs`.

### `substituters[].max_concurrent_requests`

- Type: Positive Integer | None
- Default: none

Per-substituter override for the maximum number of concurrent NAR file streaming requests to this substituter, useful for upstream servers that fail under high concurrency (e.g. rate-limited or self-hosted instances). The limit is keyed by the host the NAR requests are initially issued to: the `storage_url` host when configured, otherwise the `url` host; if multiple substituters share one storage host, the last configured limit wins. NAR requests whose upstream-provided URL points to a third-party host are not covered by this limit and keep the default one. When unset, falls back to `network.max_concurrent_requests`.
