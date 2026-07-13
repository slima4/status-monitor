// Monitor-detail charts. With one region in play (a region filter, or a single
// region overall) both charts scope to it: p50/p95/p99 over time, and the phase
// breakdown over time. When the org spans regions and none is filtered, each
// switches to its cross-region form via `data-overlay-endpoint` — a median line
// per region, and a stacked phase bar per region.
// Both refresh when the live KPI region settles so they track new checks.

import { initChart, renderEmptyChart, fetchJson, slidingWindow } from "./_init.js";
import { buildLatencyOption, buildLatencyOverlayOption } from "./_latency_core.js";
import { buildBreakdownOption, buildBreakdownByRegionOption } from "./_breakdown_core.js";

const LIVE_REGION = "detail-live-kpi";
const EMPTY = "# no data in this range yet";

// Both cross-region charts read the same by-region endpoint, and they refresh in
// lockstep. Sharing the in-flight promise keeps that one request, not two.
const inflight = new Map();
function fetchShared(url) {
    let p = inflight.get(url);
    if (!p) {
        p = fetchJson(url).finally(() => inflight.delete(url));
        inflight.set(url, p);
    }
    return p;
}

// One self-contained chart: its own sliding-window query + option builder, so
// the latency overlay and the merged breakdown can read different endpoints.
function makeChart(el, endpoint, build, onPick) {
    const query = slidingWindow(endpoint);
    let inst = null;
    async function refresh() {
        const { url, from, to } = query();
        let data;
        try {
            data = await fetchShared(url);
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
        if (!inst) {
            el.innerHTML = "";
            inst = initChart(el);
            if (onPick) inst.on("click", p => onPick(p));
        }
        inst.setOption(option, { notMerge: true });
    }
    return { refresh, resize: () => inst && inst.resize() };
}

// A bar click filters to that region by driving the matching row's link, which
// only exists when the bars do (both render on the same multi-region condition).
function pickRegion(params) {
    const id = params?.data?.regionId;
    if (!id) return;
    document.querySelector(`[data-region-row][data-region="${CSS.escape(id)}"] a`)?.click();
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
        const byRegion = breakdownEl.dataset.overlayEndpoint;
        charts.push(byRegion
            ? makeChart(breakdownEl, byRegion, d => buildBreakdownByRegionOption(d.regions ?? []), pickRegion)
            : makeChart(breakdownEl, breakdownEl.dataset.endpoint, (d, f, t) => buildBreakdownOption(d.buckets ?? [], f, t)));
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
