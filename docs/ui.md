# Web UI

The same Rust binary that serves `/api/v1/*` also serves a server-rendered HTML UI on the same port. Open `http://<host>:<api-port>/` in a browser.

## Stack

| Layer | What | Where |
|---|---|---|
| Templates | askama 0.16 + askama_web 0.16 (compile-time HTML, type-checked by `cargo build`) | `templates/` |
| Interactivity | HTMX 2.0.9 + json-enc (partial swaps, JSON form submission — no SPA framework) | `static/js/htmx.min.js`, `static/js/json-enc.js` |
| Charts | ECharts 6 (lazy-loaded only on pages that need it) | `static/js/echarts.min.js`, `static/js/charts/` |
| CSS | Tailwind 4.3 (CSS-first config via `@import`, `@source`, `@theme`, `@layer`) | `static/css/input.css` → `app.css` |
| Asset serving | `rust-embed` — assets are baked into the binary at compile time | `src/web/assets.rs` |

After `cargo build --release` you have one ~23 MB executable that contains every template, every CSS byte, and every vendored JS file. No node, no bundler, no separate frontend service.

## Routes

| Path | Purpose |
|---|---|
| `GET /` | Dashboard. Auto-refreshing region polls `/web/partials/dashboard` every 5 s; donut + 24h bar pull from `/api/v1/dashboard/summary`. |
| `GET /targets` | Targets list. Filter by name (client-side), tag, enabled. Row delete + paginate via HTMX. Rows authored by an API token or Terraform carry a `managed-chip` (`api` / `terraform`); UI-authored rows show none. |
| `GET /targets/{id}` | Target detail. Status badge, four time-range presets (1h/24h/7d/30d), uptime KPIs, latency p50/p95/p99 line, DNS/connect/TLS/TTFB stacked area, recent-results table, redacted JSON config. Externally-managed monitors also get a managed-by chip and a banner warning that UI edits may be overwritten on the next apply. |
| `GET /targets/new` | Create form. Posts JSON to `/api/v1/targets`. Detection (open-incident-after-N-fails, region quorum) and Notifications (channel bindings, remind-while-down cadence, notify-on-recovery) are separate sections; the notification controls only render when the org has channels. |
| `GET /targets/{id}/edit` | Edit form. Same template as `new` but `data-mode="edit"`; credential fields land in `redacted` mode and the operator must explicitly toggle "Replace credentials" before new values are sent. |
| `GET /web/targets/list` | HTMX partial (`<tbody>` fragment) for filter/paginate swaps on the targets list. |
| `GET /settings/notifications` | Notification-channel list. Send-test / edit / delete are HTMX row actions against `/api/v1/notification-channels`; the table body polls `/web/partials/settings/notifications` every 60 s. |
| `GET /settings/notifications/new`, `…/{id}/edit` | Channel create/edit form (Slack / Discord / Teams / Google Chat / generic webhook / Telegram / WhatsApp; the provider-branded cards take just the provider's webhook URL, host-checked on create). With provider OAuth configured, the slack/discord panel grows an "add to Slack" / "add to Discord" button (plus a QR variant for a signed-in phone): the provider's consent screen picks the destination channel and the callback lands on the ready-made channel's edit page; cancelling, a failed exchange, or the plan's channel limit bounce back to the form with a quiet note. On deployments running the central Telegram bot, a one-tap **telegram** card joins the lineup (the BYO card reads **telegram bot**): "connect telegram" mints a single-use code, shows it as a t.me link + QR with a private-chat/group toggle, polls until the chat presses Start, then opens the channel the webhook created. Linked channels are display-only (chat title + id, no secrets, no replace toggle); unlink = delete. If the chat side unlinks first (bot removed, `/stop`), the channel is disabled with a visible "unlinked from the Telegram side" note that re-enabling clears. The Telegram panel has a setup helper: a t.me QR for the bot (scan, press Start) and a one-click chat-id probe, both talking to the Bot API straight from the browser. "Test now" delivers a synthetic alert before saving (create posts the form config to `…/test`; a locked edit tests the stored channel by id). On edit the stored secret stays masked behind a "Replace transport config" toggle — leaving it off omits `config` from the PATCH and locks the type cards (the kind rides the config). The edit page also lists the monitors bound to the channel, lets a "+ add monitor" picker bind more (it updates the monitor's alert bindings through `PATCH /api/v1/targets/{id}`), and offers delete with that blast radius spelled out — deleting a channel also removes its bindings from every monitor. |
| `GET /settings/pages` | Status-pages list — create / rename / publish / delete pages (free plan: one). Create posts to `/api/v1/status-pages`; the list body refreshes via `/web/partials/settings/pages`. |
| `GET /settings/pages/{id}` | Per-page editor: URL slug (own save — a rename is a hard cutover), branding, logo, and the component curation list (per-monitor on-page toggle, public name/group). Each edit autosaves via the `/api/v1/status-pages/{id}` + `/components` endpoints. |
| `GET /settings/team` | Team management (owner-only): invite by email + role, pending-invitation revoke, member remove / leave, owner⇄member role toggle — all row actions confirm via modal and hit `/api/v1/orgs/{id}/members` + `/api/v1/orgs/{id}/invitations`. Non-owner members see a read-only note. |
| `GET /web/partials/settings/team` | HTMX partial — seats line + members + pending-invitations tables; re-pins the target org id on every refresh. |
| `GET /web/partials/settings/pages` | HTMX partial — the page rows for the list above. |
| `GET /web/partials/dashboard` | HTMX partial — chrome-free dashboard region; self-rearms so each refresh still carries `hx-trigger="every 5s"`. |
| `GET /m/{token}` | Public read-only share of one monitor — same detail dashboard, no operator chrome, credentials redacted, no login. Sub-resources (`/live`, `/incidents`, `/latency`, `/results`) are twinned under the token so the page never calls an operator URL. See [Share links](share-links.md). |
| `GET /docs` | Swagger UI generated from `/api/openapi.json`. |
| `GET /static/{path}` | Embedded assets (`css/`, `js/`, `img/`). |

Every mutation goes through `/api/v1/*`. There are **no** `/web/*` write routes — the JSON API stays the single source of truth, which means a future SvelteKit port is a templates-only rewrite. The `/m/{token}` share surface is read-only and serves no write method.

## Build pipeline

```
cargo build [--release]
   └─► build.rs
         ├─► (first build only) scripts/fetch-tailwind.sh — downloads the Tailwind
         │     standalone CLI (~30 MB, not committed) for the host platform into bin/
         └─► ./bin/tailwindcss --minify
                --input  static/css/input.css
                --output static/css/app.css
   └─► rustc
         └─► rust-embed bakes static/ + templates/ into the binary
```

`build.rs` declares `rerun-if-changed` on `templates/`, `src/`, `static/css/input.css`, and `scripts/fetch-tailwind.sh`. Editing any of them triggers a Tailwind rebuild on the next `cargo build`.

Tailwind 4 scans both `templates/**/*.html` and `src/**/*.rs` for utility class names (declared via `@source` in `input.css`), so utility classes written inside Rust strings are preserved through tree-shaking.

## Styling: the semantic layer

`static/css/input.css` is layered: design tokens (`@theme`, e.g. `--color-ink`) → primitives (`.sticker-card`, `.sticker-btn`, `.sticker-pill`) → **semantic classes** (`.page-title`, `.panel-label`, `.kpi-value`, `.stat-tile`, `.status-badge--*`, `.btn-ghost`, `.sticker-btn--primary/--danger`, `.nav-link`, `.day-cell`). Templates reference **only** the semantic names — no raw colour/shape utility clusters. State is one `--modifier` (`.status-badge--down`, `.stat-tile--ok`). Result: re-skinning the internal app is an `input.css`-only edit, no template touched. When adding UI, reuse/extend a semantic class rather than inlining `bg-*`/`rounded-*`/heading-scale clusters. The public status page is deliberately exempt — it's a flat, brand-themed surface with its own view-supplied palette (`public_status.rs`), not the cartoon sticker system.

## Dashboard refresh model

The dashboard splits into three regions:

1. **Chrome** (nav, page header) — rendered once.
2. **Auto-refresh region** (`<div id="dashboard-region">`) — KPI cards + system-health card. Polls `/web/partials/dashboard` every 5 s and swaps its own outer HTML so the trigger remains armed.
3. **Charts** (donut + 24h composition bar) — placed *outside* the refresh region so the ECharts instances persist across polls. The chart wrapper listens for `htmx:afterSettle` on the region and re-fetches `/api/v1/dashboard/summary` once per cycle, fanning out to both charts (single network round-trip, not one per chart).

The `dashboard_summary` handler caches its result in `state.dashboard_cache` for 5 s, so the polling load on Postgres + ClickHouse is bounded to one query set per 5 s regardless of how many tabs are open.

## Credential redaction

For `basic_auth` and `bearer_token` the form runs a three-state machine in `static/js/ui/auth_field.js`:

| `data-mode` | Inputs | Submit behaviour |
|---|---|---|
| `create` | enabled, empty | Field included in POST body if filled. |
| `redacted` | disabled, sentinel `***` shown | Field **omitted** from PATCH body. |
| `replacing` | enabled, empty | Field included with the real value. |

The API rejects the `***` sentinel on write as defence-in-depth — but the state machine prevents the form from ever submitting it. End-to-end coverage in `tests/web_e2e_test.rs::edit_form_shows_redacted_auth_state_for_existing_target` asserts that real credentials never appear in the rendered edit form.

## Tests

| Layer | What |
|---|---|
| Unit (template render) | Every view in `src/web/views/` ships a `#[test]` that renders the template with a fixtures struct and asserts on the output: HTMX hooks, redaction sentinels, chart `data-endpoint`s, table scaffolding. |
| End-to-end | `tests/web_e2e_test.rs` drives the merged api+web router via `tower::ServiceExt::oneshot`, covering dashboard (full + partial), list (full + partial), forms (create + redacted-edit), target detail with chart anchors + time-range nav, 404 paths, and the immutable cache header on `/static/*`. |
| Build-time | `cargo build` rejects template type mismatches — askama checks templates against the corresponding Rust struct at compile time. |

```bash
cargo test --lib web::          # unit render tests
cargo test --test web_e2e_test  # end-to-end
```

## Adding a new page

1. Add a template under `templates/` extending `base.html`.
2. Add a `#[derive(Template, WebTemplate)]` struct and an axum handler in `src/web/views/`.
3. Register the route in `src/web/routes.rs`.
4. Tailwind picks up new utility classes automatically (the `@source` directive scans `templates/**/*.html` + `src/**/*.rs`).
5. Add a render test next to the view and, if there's a route worth covering end-to-end, append a case to `tests/web_e2e_test.rs`.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `failed to spawn ./bin/tailwindcss` during `cargo build` | First-build fetch failed. Run `bash scripts/fetch-tailwind.sh` manually and confirm `bin/tailwindcss` is executable. |
| Page renders unstyled HTML | `static/css/app.css` empty or stale. Touch `static/css/input.css` and rebuild; the build script runs Tailwind with `--minify`. |
| Charts render blank | Open DevTools console. Most likely a fetch to `/api/v1/dashboard/summary` or `/api/v1/targets/{id}/results` failed — the chart module logs `chart load failed` with the URL and status. |
| Dashboard never refreshes | Confirm `<script defer src="/static/js/htmx.min.js">` is in the page source. The HTMX bundle is loaded from `base.html`. |
| Edit form submitted credentials despite the toggle being off | Look for a console error from `auth_field.js`. The submit handler reads `data-mode` from the credential `<fieldset>` — if the fieldset is missing the data attributes, it will fall back to "include". |

## Migrating to a SPA later

The design keeps a SPA port cheap. Every `templates/*.html` maps one-to-one to a Svelte (or React) component, every chart module under `static/js/charts/` is already a pure `(element, endpoint) → disposer` function that imports unchanged into `onMount`, and there are zero `/web/*` write endpoints to refactor — only read partials. To swap frameworks:

1. Generate a typed JSON client from `/api/openapi.json`.
2. Port the templates page-by-page; keep `/api/v1/*` unchanged.
3. Drop `src/web/views/` (keep `src/web/assets.rs` pointing at the new bundle).
4. Delete `templates/` and `static/js/{htmx,json-enc,ui}` — no longer needed.

The backend (`src/api/`, `src/storage/`, `src/scheduler/`, `src/worker/`) stays untouched.
