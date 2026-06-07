// Monitor-detail charts. The latency chart draws p50/p95/p99 for the selected
// region, OR — when the org spans regions and no single region is filtered — a
// per-region p95 overlay (the `data-overlay-endpoint`). The breakdown chart is
// always the merged phase view (region-filtered server-side when one is picked).
// Both refresh when the live KPI region settles so they track new checks.

import { initChart, renderEmptyChart, fetchJson, slidingWindow } from "./_init.js";
import { buildLatencyOption, buildLatencyOverlayOption } from "./_latency_core.js";
import { buildBreakdownOption } from "./_breakdown_core.js";

const LIVE_REGION = "detail-live-kpi";
const EMPTY = "No data in this range yet.";

// One self-contained chart: its own sliding-window query + option builder, so
// the latency overlay and the merged breakdown can read different endpoints.
function makeChart(el, endpoint, build) {
    const query = slidingWindow(endpoint);
    let inst = null;
    async function refresh() {
        const { url, from, to } = query();
        let data;
        try {
            data = await fetchJson(url);
        } catch (err) {
            console.warn(`${url} chart load failed`, err);
            return;
        }
        const option = build(data, from, to);
        const empty = !option.series || option.series.every(s => !s.data || s.data.length === 0);
        if (empty) {
            if (inst) { inst.dispose(); inst = null; }
            renderEmptyChart(el, EMPTY);
            return;
        }
        if (!inst) { el.innerHTML = ""; inst = initChart(el); }
        inst.setOption(option, { notMerge: true });
    }
    return { refresh, resize: () => inst && inst.resize() };
}

function start() {
    const charts = [];

    const latencyEl = document.querySelector("#latency-chart[data-endpoint]");
    if (latencyEl) {
        const overlay = latencyEl.dataset.overlayEndpoint;
        charts.push(overlay
            ? makeChart(latencyEl, overlay, (d, f, t) => buildLatencyOverlayOption(d.regions ?? [], f, t))
            : makeChart(latencyEl, latencyEl.dataset.endpoint, (d, f, t) => buildLatencyOption(d.buckets ?? [], f, t)));
    }

    const breakdownEl = document.querySelector("#breakdown-chart[data-endpoint]");
    if (breakdownEl) {
        charts.push(makeChart(breakdownEl, breakdownEl.dataset.endpoint, (d, f, t) => buildBreakdownOption(d.buckets ?? [], f, t)));
    }

    if (charts.length === 0) return;

    charts.forEach(c => c.refresh());
    window.addEventListener("resize", () => charts.forEach(c => c.resize()), { passive: true });
    document.body.addEventListener("htmx:afterSettle", (e) => {
        if (e.detail && e.detail.target && e.detail.target.id === LIVE_REGION) {
            charts.forEach(c => c.refresh());
        }
    });
}

if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
} else { start(); }
