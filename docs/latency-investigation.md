# Latency and incomplete investigation (2026-09-27)

This is an investigation branch, not a completed TLS fix or a release. Production deployment is paused because another collaborator is updating the live service. No production binary from this branch has been deployed.

## Changes in this branch

- Reuse HTTP clients and their connection pools per account. Re-read the account map before each request; changed routes replace the client and removed accounts remain rejected. No credentials are stored in default client headers.
- Log preparation, upstream response headers, first event, terminal/error timing, and the last event before a premature EOF. Request IDs correlate these records with CPR.
- Remove logging of complete upstream response headers.
- Provide an opt-in, bounded capture of failed function-call translation for diagnosis. Normal operation captures no payloads.

## Optional diagnostic capture

Place `diagnostics.json` beside the account map. It contains `key_hash` (the SHA-256 digest of the specific test API key) and `expires` (Unix seconds, at most 30 minutes ahead). Only signed requests matching that key are eligible. Up to eight failed `response.output_item.done` function-call events, each no more than 128 KiB, are saved as `tool-diagnostic-0.json` through `tool-diagnostic-7.json` with mode 0600. Existing files are not overwritten. Credentials, headers and full input requests are not captured, but tool arguments can contain sensitive text; use synthetic test requests and retain these files privately. Replace the configuration with `{}` to disable capture immediately. Expiry disables capture automatically. Do not commit captured files.

## Evidence and remaining work

- Actual Codex reported TLS handshake EOF while reconnecting to the public relay domain. This identifies the client-to-relay connection, but does not yet prove which network component closed it.
- The local domain is routed through ClashMac TUN and an AI proxy group. Fifteen TLS/HTTP probes through the public domain and fifteen to the origin, with certificate validation enabled, all succeeded. This does not exclude an intermittent fault on long-lived connections.
- Some production requests established the upstream WebSocket in under half a second, then failed tool-envelope translation after more than 200 seconds. These are not explained by admission queue time alone. Capture and reproduce the actual malformed envelope before widening parser acceptance.
- Upstream failure/incomplete reason propagation, real Codex long-conversation regression, and before/after latency measurements remain unfinished. Do not treat an HTTP 200 as proof that a response completed.

Validation for this checkpoint: workspace tests, including a real local HTTP connection reuse test and route-revocation assertions; workspace Clippy with warnings denied. No dependency changes. Full integration verification is still in progress.
