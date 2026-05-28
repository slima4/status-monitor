// Both detail charts read the same results window, so they share one fetch
// per refresh. Refresh fires when the live KPI region settles.

import { initChart, renderEmptyChart, fetchJson, slidingWindow } from "./_init.js";
import { buildLatencyOption } from "./_latency_core.js";
import { buildBreakdownOption } from "./_breakdown_core.js";

const LIVE_REGION = "detail-live-kpi";
const EMPTY = "No data in this range yet.";

function start() {
    const specs = [
        { el: document.querySelector("#latency-chart[data-endpoint]"), build: buildLatencyOption },
        { el: document.querySelector("#breakdown-chart[data-endpoint]"), build: buildBreakdownOption },
    ].filter(s => s.el);
    if (specs.length === 0) return;

    const query = slidingWindow(specs[0].el.dataset.endpoint);
    const instances = new Map();

    async function refresh() {
        const { url, from, to } = query();
        let buckets;
        try {
            buckets = (await fetchJson(url)).buckets ?? [];
        } catch (err) {
            console.warn(`${url} chart load failed`, err);
            return;
        }
        for (const { el, build } of specs) {
            if (buckets.length === 0) {
                const inst = instances.get(el);
                if (inst) { inst.dispose(); instances.delete(el); }
                renderEmptyChart(el, EMPTY);
                continue;
            }
            let inst = instances.get(el);
            if (!inst) { el.innerHTML = ""; inst = initChart(el); instances.set(el, inst); }
            inst.setOption(build(buckets, from, to), { notMerge: true });
        }
    }

    refresh();
    window.addEventListener("resize", () => instances.forEach(i => i.resize()), { passive: true });
    document.body.addEventListener("htmx:afterSettle", (e) => {
        if (e.detail && e.detail.target && e.detail.target.id === LIVE_REGION) refresh();
    });
}

if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
} else { start(); }
