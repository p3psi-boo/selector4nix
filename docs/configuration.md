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

Fastly endpoint and SNI proxy optimization for automatically detected Fastly substituters. This is a global section and does not belong to any single substituter.

When either CDN optimization section is enabled, selector4nix classifies every unique configured substituter host at startup. It first queries A and CNAME records through both DoH resolvers, compares returned addresses with the official [Cloudflare IPv4 ranges](https://www.cloudflare.com/ips-v4/) and [Fastly public IP list](https://api.fastly.com/public-ip-list), and recognizes the corresponding CDN CNAME suffixes. If DNS evidence is inconclusive, it requests the substituter's `nix-cache-info` and checks Cloudflare or Fastly response headers. Conflicting or unknown evidence is left unclassified and uses the normal system-DNS path. Detection results and their evidence are written to the log.

When `fastly_optimization.enabled` is true, every substituter classified as Fastly is sent through Fastly edge endpoints and SNI proxies discovered and probed at runtime. Hostnames are not hard-coded: `cache.nixos.org` is one normal detected Fastly host, and other Fastly-backed substituters are handled the same way. Only the TCP connection target IP is overridden (equivalent to `curl --resolve`); each substituter's request URL, HTTP `Host` header, TLS SNI, and strict certificate verification remain unchanged.

Candidate endpoints come from four sources: DNS-over-HTTPS lookups against `cloudflare-dns.com` and `dns.google` (bypassing the system DNS, so fake-ip environments work too), IP literals configured via `candidates`, region-derived candidates when `derive_regions` is enabled, and Fastly-specific SNI proxy IP lists configured in `sni_proxy_sources`. SNI proxy list entries are never mixed with the Cloudflare platform. Every candidate must pass an end-to-end admission probe — a TLS handshake plus `GET /nix-cache-info` through that IP — before it is considered usable. The original URL, Host, SNI, and certificate verification remain intact, so an SNI proxy is admitted only when it forwards the original TLS connection correctly.

Newly admitted and stale endpoints are actively benchmarked with a bounded HTTPS download. Endpoints are ordered by estimated 10 MiB download time using measured TTFB and throughput; NAR bytes are discarded instead of cached. A failed benchmark does not revoke admission. A failing request automatically tries another endpoint and then the default system-DNS path.

Note that endpoint requests do not use HTTP(S)_PROXY or other environment proxies; they connect directly to the selected IP. Under transparent proxies (TUN/fake-ip), all endpoints go through the same proxy chain, so endpoint preference is of limited benefit there — DoH-based discovery still works in that case. All Range requests of a chunked NAR download are pinned to the same endpoint, and the concurrency quota is still shared per logical host.

Enabling this section does not require a particular hostname. If no configured substituter is detected as Fastly, this section remains idle and changes no request path.

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

### `fastly_optimization.sni_proxy_sources`

- Type: Array of Tables
- Default: `[]`

Fastly-specific plain-text SNI proxy IP sources. Supported URL schemes are `file://`, `http://`, and `https://`. Each non-empty line contains one IPv4 or IPv6 address; text after `#` is ignored. Domains, CIDRs, and `IP:port` entries are ignored. Results are cached in memory until `refresh_secs`; the last successful result remains usable after a refresh error.

Each listed IP is connected on the substituter's normal HTTPS port. The proxy must route the untouched ClientHello according to SNI and must not terminate TLS.

#### `fastly_optimization.sni_proxy_sources[].url`

Location of the IP list. A `file://` URL must be absolute. HTTP and HTTPS sources are fetched directly without environment proxies.

#### `fastly_optimization.sni_proxy_sources[].refresh_secs`

- Type: Positive Integer
- Default: `3600`

Minimum interval between source reloads.

### `fastly_optimization.bandwidth_probe`

Active bounded-download benchmark settings. The default URL is an immutable `cache.nixos.org` NAR larger than the default 10 MiB sample.

#### `fastly_optimization.bandwidth_probe.enabled`

- Type: Boolean
- Default: `true`

#### `fastly_optimization.bandwidth_probe.url`

- Type: HTTPS URL
- Default: `https://cache.nixos.org/nar/1as5cn000kck2y35awm3825qvlvcnq08jbilwdlzdlv6pbidk3i4.nar.zst`

#### `fastly_optimization.bandwidth_probe.bytes`

- Type: Positive Integer
- Default: `10485760`

#### `fastly_optimization.bandwidth_probe.refresh_secs`

- Type: Positive Integer
- Default: `21600`

#### `fastly_optimization.bandwidth_probe.max_concurrent_probes`

- Type: Positive Integer
- Default: `2`

## `cloudflare_optimization`

Cloudflare endpoint and SNI proxy optimization for automatically detected Cloudflare substituters. This is a global section and does not belong to any single substituter.

When enabled, every substituter classified as Cloudflare is sent through admitted Cloudflare edge or SNI proxy IPs. This includes Cachix hosts, but is not limited to a hostname suffix. Only the TCP connection target IP is overridden; the request URL, HTTP `Host`, TLS SNI, and strict certificate verification remain unchanged.

Candidate endpoints come from DoH lookups of the substituter, DoH lookups of `discovery_domains`, configured IP literals, and Cloudflare-specific `sni_proxy_sources`. There is no Fastly region derivation. Every candidate must pass the actual substituter host's TLS and `/nix-cache-info` admission probe. Admitted endpoints are benchmarked through the configured Cloudflare test URL and ordered by estimated download time.

The default `discovery_domains` entry, `cloudflare.182682.xyz`, is a third-party-maintained list of optimized Cloudflare IPs. It is not operated by this project and may change or become unavailable at any time; because every candidate must pass the admission probe anyway, a stale or dead discovery domain merely yields fewer candidates and never affects correctness.

Endpoint requests do not use HTTP(S)_PROXY or other environment proxies; they connect directly to the selected IP. All Range requests of a chunked NAR download are pinned to the same endpoint, and the concurrency quota is still shared per logical host — same as `fastly_optimization`. Endpoint status is shown on the dashboard overview page.

Enabling this section does not require a particular hostname. If no configured substituter is detected as Cloudflare, this section remains idle and changes no request path.

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

### `cloudflare_optimization.sni_proxy_sources`

- Type: Array of Tables
- Default: `[]`

Cloudflare-specific SNI proxy IP lists. The format, caching, and admission rules are the same as `fastly_optimization.sni_proxy_sources`, but the two platform lists are stored and loaded separately.

#### `cloudflare_optimization.sni_proxy_sources[].url`

- Type: `file://`, `http://`, or `https://` URL

Location of the plain-text IP list.

#### `cloudflare_optimization.sni_proxy_sources[].refresh_secs`

- Type: Positive Integer
- Default: `3600`

How long the in-memory result is reused before fetching the list again. List refreshes are evaluated during the endpoint manager's approximately 30-minute refresh cycle, so shorter values take effect on that next cycle.

### `cloudflare_optimization.bandwidth_probe`

Active bounded-download benchmark settings. By default the URL is generated as `https://speed.cloudflare.com/__down?bytes=BYTES`, where `BYTES` is the configured sample size. The benchmark keeps Host and SNI as `speed.cloudflare.com` while connecting to the candidate IP, which verifies and measures the candidate's Cloudflare path.

#### `cloudflare_optimization.bandwidth_probe.enabled`

- Type: Boolean
- Default: `true`

#### `cloudflare_optimization.bandwidth_probe.url`

- Type: HTTPS URL
- Default: `https://speed.cloudflare.com/__down?bytes=10485760`

The response must contain at least the configured number of bytes. The client requests a bounded Range and stops after collecting the sample.

#### `cloudflare_optimization.bandwidth_probe.bytes`

- Type: Positive Integer
- Default: `10485760`

#### `cloudflare_optimization.bandwidth_probe.refresh_secs`

- Type: Positive Integer
- Default: `21600`

#### `cloudflare_optimization.bandwidth_probe.max_concurrent_probes`

- Type: Positive Integer
- Default: `2`

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

### Runtime changes

The substituter list in the configuration file is the restart source of truth. After the process has loaded that file, the dashboard overview page can add a substituter or enable/disable an existing one without rewriting the file. This is the supported way to change upstream mirrors when the configuration is read-only, such as a Nix store path generated by the NixOS, nix-darwin, or Home Manager modules.

Runtime changes take effect immediately for new NAR info lookups and NAR downloads. They are kept only in memory and are discarded when the process exits. To make a change permanent, add it to the configuration file.

The dashboard exposes the following `POST` endpoints, accepting `application/x-www-form-urlencoded` bodies:

- `/dashboard/substituters` with `url` (required), `priority` (optional, default `40`), and `storage_url` (optional)
- `/dashboard/substituters/enable` with `url`
- `/dashboard/substituters/disable` with `url`

At least one substituter must remain enabled. A substituter added at runtime uses the system DNS path unless its host already has an endpoint manager from startup CDN detection.
