// Detail-page wiring: Run-check-now button, Enable/Disable toggle, and a
// one-shot nudge that resolves a just-created monitor's first status.
//
// The KPI cards + Recent results table live inside `#detail-live-kpi`, an
// htmx-polled partial that re-renders every 60s (or on demand via the
// `sm:refresh-live` body event). We don't re-render those regions
// client-side — the server template is the single source of truth.

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
                    footnote: "Metrics and charts update automatically.",
                });
                refreshLive();
            } finally {
                btn.disabled = false;
            }
        });
    }

    // Region filter: full-page nav (so the chart modules re-init) preserving
    // the current range. Empty value clears the filter back to all regions.
    const regionSel = document.querySelector("[data-region-filter]");
    if (regionSel) {
        regionSel.addEventListener("change", () => {
            const url = new URL(location.href);
            if (regionSel.value) url.searchParams.set("region", regionSel.value);
            else url.searchParams.delete("region");
            location.assign(url.pathname + url.search);
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
                        "X-Requested-With": "uptimepage",
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

    // Result-row timing expansion: delegated from document so it works for
    // server-rendered rows in the ribbon drill drawer and the share table.
    document.addEventListener("click", (ev) => {
        const row = ev.target.closest("[data-result-row]");
        if (!row) return;
        const detail = row.nextElementSibling;
        if (!detail || !detail.hasAttribute("data-result-detail")) return;
        const open = detail.classList.toggle("hidden");
        row.setAttribute("aria-expanded", String(!open));
    });

    // Header ⋯ overflow menu: native <details> stays open on outside click, so
    // dismiss it on any click outside or Escape.
    const closeHdrMenus = (except) => {
        document.querySelectorAll("details.hdr-menu[open]").forEach((d) => {
            if (d !== except) d.removeAttribute("open");
        });
    };
    document.addEventListener("click", (ev) => {
        closeHdrMenus(ev.target.closest("details.hdr-menu"));
    });
    document.addEventListener("keydown", (ev) => {
        if (ev.key === "Escape") closeHdrMenus(null);
    });

    // A just-created monitor has no result until its first check lands.
    // Poll until it does so "checking…" resolves without waiting for the
    // 60s cadence, then stop — no steady-state extra requests.
    const lastCheck = document.querySelector("[data-last-check]");
    if (lastCheck && lastCheck.dataset.enabled === "true" && !lastCheck.dataset.lastAt) {
        const nudge = setInterval(() => {
            if (document.visibilityState === "visible") refreshLive();
        }, 5000);
        document.body.addEventListener("htmx:afterSettle", (ev) => {
            const target = ev.detail && ev.detail.target;
            if (target && target.id === "detail-live-kpi" && target.dataset.newestTs) {
                clearInterval(nudge);
            }
        });
    }
})();
