# Gateway console rescue

Scope: rescue both monitoring and settings UI plus their plugin management endpoints. Keep the official CPR host unchanged and preserve authentication, signed control calls, existing policy version checks and busy-request guards. Per-Key routing is explicitly outside this change.

Design: a compact operations console using the existing CPR theme, a clear title and connection state, a two-view navigation, meaningful recent-record metrics, searchable/paged request rows and expandable error details. Settings use grouped sections for model channels, capacity/queue and account scope, with a sticky save bar, dirty-state protection and explicit conflict recovery. Loading, empty, disconnected and stale-data states remain visible without destroying existing content. Narrow layouts and light/dark themes use the same components.

Implementation order:
1. Add individually paginated read-only Key/account endpoints and clear management errors; preserve the existing options endpoint. Separate the frontend transport and reactive state from page rendering.
2. Replace the monolithic page with monitoring and settings components and one scoped visual system. Preserve drafts across refresh and tab changes, support discard/reload, and distinguish bridge availability from Excel enabled state.
3. Run focused endpoint/state checks, normal build/lint and browser interaction checks (monitor, details, filters, settings save, dirty refresh, narrow/dark). Reuse the existing external rollback evidence, then commit and push. Do not deploy without a separate request.

No new product hashes, frozen contracts, release gates, speculative retries or extra frameworks.
