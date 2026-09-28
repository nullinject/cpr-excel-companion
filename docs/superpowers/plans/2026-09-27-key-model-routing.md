# Key-centred gateway routing implementation

Goal: select a Client Key, configure model channel overrides, and keep routine configuration in one gateway page.

Constraints: preserve CPR authorization and signed account isolation; never infer request identity from observed traffic or read API-key plaintext. No speculative fallback routes. Keep existing global defaults and suffix compatibility. Use existing policy persistence/version handling, not new locks or contracts.

1. Do not modify CPR. Reuse and verify the pre-0.3 native binding mechanism, then connect Key-specific model rules through supported host facilities. Preserve first-request correctness and restart behaviour.
2. Add a searchable Key selector and model table showing inherit/Excel/native, actual resolution and unavailable reasons. Keep global defaults accessible; group concurrency and connection diagnostics into clearly named sections. Read-only infrastructure state must not masquerade as editable settings.
3. Exercise only focused routing checks plus normal Rust/frontend builds. Preserve existing transaction evidence outside Git. Update obsolete routing documentation, commit and push the implementation promptly.

No server deployment or permission expansion is included in this UI/routing change without a verified supported host interface.

## Verified scope correction

User explicitly rejected modifying CPR. v0.1.1 exposes a Key selector but compares its whitelist with static isolationScope; native host bindings are a separate working scope filter. Official middleware has one binding per stage and does not expose the authenticated Key ID. The management frame has no host binding mutation API.

Ship working global model/channel controls, grouped daily settings, clear native binding guidance, and removal of dead Authorization extraction. Preserve existing policy data and host authorization. The independent per-Key matrix remains unfinished; do not mark it delivered or deploy a patched host.
