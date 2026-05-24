// Detail-page wiring: Run-check-now button, Enable/Disable toggle, and
// the "checked Ns ago · next in Ns" ticker.
//
// The KPI cards + Recent results table live inside `#detail-live`, an
// htmx-polled partial that re-renders every 60s (or on demand via the
// `sm:refresh-live` body event). We don't re-render those regions
// client-side — the server template is the single source of truth, so
// no row/badge/timestamp formatters here.

(function () {
    function refreshLive() {
        // Tell the #detail-live-kpi partial to refresh now. Safe no-op
        // if htmx isn't on the page or the region isn't mounted yet.
        if (window.htmx && document.getElementById("detail-live-kpi")) {
            window.htmx.trigger("body", "sm:refresh-live");
        }
    }

    // Pause #detail-live-kpi polling while the tab is hidden. A user
    // who left the tab open in the background doesn't need fresh KPIs
    // until they come back, so silencing the every-60s + ticker-driven
    // refresh kills a sustained per-viewer request the server doesn't
    // have to serve. A single catch-up refresh fires on becoming
    // visible again. Manual paths (Run check now, Enable/Disable) are
    // user-initiated → by definition fire while visible, so they're
    // never blocked by the interceptor.
    document.body.addEventListener("htmx:beforeRequest", (ev) => {
        const elt = ev.detail && ev.detail.elt;
        if (!elt || elt.id !== "detail-live-kpi") return;
        if (document.visibilityState === "hidden") {
            ev.preventDefault();
        }
    });
    document.addEventListener("visibilitychange", () => {
        if (document.visibilityState === "visible") refreshLive();
    });

    // Run check now: persist a fresh check, render the verbose result
    // pill, then ask the live partial to pick up the new row + uptime.
    const btn = document.querySelector("[data-detail-test-now]");
    const resultEl = document.querySelector("[data-detail-test-result]");
    if (btn && resultEl) {
        btn.addEventListener("click", async () => {
            const id = btn.dataset.targetId;
            if (!id) return;
            btn.disabled = true;
            window.smRenderCheckRunning(resultEl);
            try {
                const r = await window.smRunCheckNow(id);
                if (!r.ok) {
                    const code = (r.body && r.body.error && r.body.error.code)
                        || (r.networkError ? "network" : `HTTP ${r.status}`);
                    const message = (r.body && r.body.error && r.body.error.message)
                        || (r.networkError ? String(r.networkError.message || r.networkError)
                                           : "Check rejected.");
                    window.smRenderCheckError(resultEl, `${code}: ${message}`);
                    return;
                }
                window.smRenderCheckResult(resultEl, r.body || {}, {
                    footnote: "Metrics update automatically; charts refresh on next page load.",
                });
                refreshLive();
            } finally {
                btn.disabled = false;
            }
        });
    }

    // Enable/Disable toggle.
    const toggleBtn = document.querySelector('[data-action="toggle-enabled"]');
    const toggleErr = document.querySelector("[data-detail-toggle-error]");
    if (toggleBtn) {
        toggleBtn.addEventListener("click", async () => {
            const id = toggleBtn.dataset.targetId;
            const current = toggleBtn.dataset.current === "true";
            if (!id) return;
            toggleBtn.disabled = true;
            if (toggleErr) toggleErr.classList.add("hidden");
            try {
                const r = await fetch(`/api/v1/targets/${id}`, {
                    method: "PATCH",
                    headers: {
                        "Content-Type": "application/json",
                        "Accept": "application/json",
                        "X-Requested-With": "status-monitor",
                    },
                    body: JSON.stringify({ enabled: !current }),
                });
                if (r.ok) {
                    window.location.reload();
                    return;
                }
                let body = null;
                try { body = await r.json(); } catch { /* empty */ }
                const err = (body && body.error) || {};
                const msg = `${err.code || `HTTP ${r.status}`}: ${err.message || "Toggle failed."}`;
                if (toggleErr) {
                    toggleErr.textContent = msg;
                    toggleErr.classList.remove("hidden");
                }
            } catch (err) {
                const msg = `network: ${String(err.message || err)}`;
                if (toggleErr) {
                    toggleErr.textContent = msg;
                    toggleErr.classList.remove("hidden");
                }
            } finally {
                toggleBtn.disabled = false;
            }
        });
    }

    // "checked Ns ago · next in Ns" ticker. Reads `data-last-at` from
    // the ticker span on first render and `data-newest-ts` from the
    // #detail-live partial after every htmx swap, so a freshly
    // server-rendered timestamp resets the countdown without a page
    // reload. When a check goes more than 2s past due we ask htmx to
    // re-poll, throttled so a genuinely behind scheduler can't hammer.
    const tickerEl = document.querySelector("[data-last-check]");
    let lastAt = NaN;
    let interval = NaN;
    let lastRefreshAt = 0;
    const REFRESH_MIN_GAP_MS = 5000;
    // Auto-refresh fires only when the scheduler is meaningfully late,
    // not the moment "due now" appears. The ticker UI already tells the
    // user a check is pending; trigger an extra refresh only when delay
    // becomes user-noticeable. Keeps steady-state polling to the
    // configured every-60s cadence instead of 12+ extra refreshes/min
    // on tabs where the scheduler is consistently a few seconds slow.
    const OVERDUE_GRACE_S = 10;

    function readTickerDataset() {
        if (!tickerEl) return;
        if (tickerEl.dataset.lastAt) {
            const parsed = new Date(tickerEl.dataset.lastAt).getTime();
            if (Number.isFinite(parsed)) lastAt = parsed;
        }
        const i = Number(tickerEl.dataset.intervalS) * 1000;
        if (Number.isFinite(i) && i > 0) interval = i;
    }

    function renderTicker() {
        if (!tickerEl || !Number.isFinite(interval)) return;
        const now = Date.now();
        if (!Number.isFinite(lastAt)) {
            tickerEl.textContent = `interval ${Math.round(interval / 1000)}s`;
            return;
        }
        const ago = Math.max(0, Math.round((now - lastAt) / 1000));
        const remaining = Math.round((lastAt + interval - now) / 1000);
        const nextStr = remaining > 0 ? `next in ${remaining}s` : "due now";
        tickerEl.textContent = `checked ${ago}s ago · ${nextStr}`;
        if (remaining <= -OVERDUE_GRACE_S
            && now - lastRefreshAt >= REFRESH_MIN_GAP_MS
            && document.visibilityState === "visible") {
            lastRefreshAt = now;
            refreshLive();
        }
    }

    if (tickerEl) {
        readTickerDataset();
        renderTicker();
        setInterval(renderTicker, 1000);
    }

    // After htmx swaps #detail-live-kpi, pick up the new newest-timestamp
    // so the ticker resets instead of clinging to the page-load value.
    document.body.addEventListener("htmx:afterSettle", (ev) => {
        const target = ev.detail && ev.detail.target;
        if (!target || target.id !== "detail-live-kpi") return;
        const newest = target.dataset.newestTs;
        if (!newest) return;
        const parsed = new Date(newest).getTime();
        if (Number.isFinite(parsed)) {
            lastAt = parsed;
            renderTicker();
        }
    });
})();
