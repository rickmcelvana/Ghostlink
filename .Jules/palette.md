## 2026-09-06 - Link Form Descriptions and Warnings via `aria-describedby`
**Learning:** Screen reader users navigating through complex settings forms via Tab focus do not automatically hear field descriptions or inline warning banners unless the input elements explicitly reference them using `aria-describedby` and `aria-labelledby`.
**Action:** In multi-field form components with helper text, generate deterministic IDs (e.g. `${fieldId}-desc`, `${fieldId}-warning`) and attach `aria-describedby` to `input`, `select`, and `radiogroup` containers.

## 2026-09-06 - Actionable Loading States on Icon-Only Async Buttons
**Learning:** Icon-only action buttons triggering asynchronous state updates (such as Refresh controls) need explicit `disabled={loading}` and `aria-busy={loading}` along with dynamic `aria-label` updates (`Refreshing...` vs `Refresh...`) to communicate status to screen readers and prevent accidental duplicate network requests.
**Action:** Always combine visual spinner animations on icon-only refresh buttons with `disabled={loading}`, `aria-busy={loading}`, `disabled:opacity-50`, and a dynamic `aria-label`.

## 2026-09-06 - Explicit ARIA Labels on EmptyState Action Primitives
**Learning:** Shared EmptyState action buttons often use brief visible labels for visual layout neatness (e.g. "Add Server"), which may lack full context for screen reader users when announced in isolation. Providing `aria-label={action.ariaLabel || action.label}` in shared status components allows views to supply descriptive screen reader context (e.g. "Enable calculator MCP server") while preserving concise visual layout.
**Action:** In shared UI primitives with action buttons, accept an optional `ariaLabel` property and default `aria-label` to `action.ariaLabel || action.label`.
