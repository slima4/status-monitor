// Incidents sub-tab: row-click toggle + lazy-loaded timeline drawer.

(function () {
    const TIMELINE_FETCH_LIMIT = 50;
    const PAD_MS = 5_000;
    const TAIL_PAD_MS = 30_000;

    const badEnough = (s) => s === "down" || s === "error";
    const goodEnough = (s) => s === "up" || s === "degraded";

    function fmtTime(iso) {
        const d = new Date(iso);
        return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
    }

    function pickTimeline(items, ongoing) {
        const asc = items.slice().reverse();
        const firstFailure = asc.find((it) => badEnough(it.status)) || null;
        let recovered = null;
        let lastFailure = null;
        if (firstFailure) {
            const firstIdx = asc.indexOf(firstFailure);
            for (let i = firstIdx + 1; i < asc.length; i++) {
                if (goodEnough(asc[i].status)) {
                    recovered = asc[i];
                    lastFailure = asc[i - 1];
                    break;
                }
            }
            if (!recovered) {
                lastFailure = asc.slice(firstIdx).reverse().find((it) => badEnough(it.status));
            }
        }
        if (ongoing) recovered = null;
        // Suppress duplicate row when lastFailure collapses onto firstFailure.
        if (lastFailure && firstFailure && lastFailure.timestamp === firstFailure.timestamp) {
            lastFailure = null;
        }
        return { firstFailure, lastFailure, recovered };
    }

    function rowHtml(label, ev) {
        if (!ev) return "";
        const meta = ev.response_code
            ? `HTTP ${ev.response_code}`
            : (ev.error ? ev.error : `${ev.duration_ms}ms`);
        return `<div class="grid grid-cols-[8rem_8rem_1fr] gap-3 py-0.5">
          <span class="font-mono text-xs text-quiet">${window.smEscapeHtml(label)}</span>
          <time datetime="${window.smEscapeHtml(ev.timestamp)}" class="font-mono text-xs">${window.smEscapeHtml(fmtTime(ev.timestamp))}</time>
          <span class="text-body"><span class="status-badge status-badge--${window.smEscapeHtml(ev.status)} mr-2">${window.smEscapeHtml(ev.status)}</span>${window.smEscapeHtml(meta)}</span>
        </div>`;
    }

    async function loadTimeline(row, body) {
        // `data-results-base` carries the per-surface results prefix
        // (`/api/v1/targets/{id}` operator-side, `/m/{token}` on a shared page),
        // so the same drawer works without leaking an operator id onto a share.
        const base = row.dataset.resultsBase;
        const fromIso = row.dataset.from;
        const toIso = row.dataset.to;
        const ongoing = row.dataset.ongoing === "true";

        const fromRaw = new Date(fromIso).getTime();
        const toRaw = toIso ? new Date(toIso).getTime() : Date.now();
        if (Number.isNaN(fromRaw) || Number.isNaN(toRaw)) {
            body.innerHTML = `<span class="flash-text flash-text--bad">could not load timeline: invalid timestamp on incident row</span>`;
            return;
        }
        const url = `${base}/results`
            + `?from=${encodeURIComponent(new Date(fromRaw - PAD_MS).toISOString())}`
            + `&to=${encodeURIComponent(new Date(toRaw + TAIL_PAD_MS).toISOString())}`
            + `&limit=${TIMELINE_FETCH_LIMIT}`;

        body.innerHTML = `<span class="font-mono text-xs text-quiet"># loading timeline…</span>`;
        try {
            const r = await fetch(url, { headers: { "Accept": "application/json" } });
            if (!r.ok) throw new Error(`HTTP ${r.status}`);
            const json = await r.json();
            const items = Array.isArray(json.items) ? json.items : [];
            if (items.length === 0) {
                body.innerHTML = `<span class="font-mono text-xs text-quiet"># no checks recorded in this window</span>`;
                return;
            }
            const tl = pickTimeline(items, ongoing);
            const parts = [
                rowHtml("first failure", tl.firstFailure),
                rowHtml("last failure", tl.lastFailure),
                rowHtml("recovered", tl.recovered),
            ].filter(Boolean);
            body.innerHTML = parts.join("")
                || `<span class="font-mono text-xs text-quiet"># no failures matched in this window</span>`;
        } catch (err) {
            body.innerHTML = `<span class="flash-text flash-text--bad">could not load timeline: ${window.smEscapeHtml(String(err.message || err))}</span>`;
        }
    }

    function toggleFor(incidentId, btn) {
        const detail = document.querySelector(
            `[data-incident-detail][data-for="${CSS.escape(incidentId)}"]`,
        );
        const row = document.querySelector(
            `[data-incident-row][data-incident-id="${CSS.escape(incidentId)}"]`,
        );
        if (!detail || !row) return;
        const isOpen = !detail.hasAttribute("hidden");
        if (isOpen) {
            detail.setAttribute("hidden", "");
            btn?.setAttribute("aria-expanded", "false");
            return;
        }
        detail.removeAttribute("hidden");
        btn?.setAttribute("aria-expanded", "true");
        if (detail.dataset.loaded !== "1") {
            const body = detail.querySelector("[data-incident-detail-body]");
            if (body) {
                loadTimeline(row, body);
                detail.dataset.loaded = "1";
            }
        }
    }

    document.body.addEventListener("click", (ev) => {
        const btn = ev.target.closest("[data-incident-expand]");
        if (btn) {
            ev.preventDefault();
            toggleFor(btn.dataset.incidentId, btn);
            return;
        }
        const row = ev.target.closest("[data-incident-row]");
        if (!row) return;
        if (ev.target.closest("a, button")) return;
        const id = row.dataset.incidentId;
        const sibling = document.querySelector(
            `[data-incident-expand][data-incident-id="${CSS.escape(id)}"]`,
        );
        toggleFor(id, sibling);
    });
})();
