import { initChart, fetchJson, bindResize, resolveToken } from "./_init.js";

// Status colours come from the same Tailwind tokens the .stat-tile--*
// classes use, so the donut/bar always match the tiles and a retheme is
// input.css-only. Resolved once (resolveToken explains the probe + the
// missing-token behaviour).
const T = {
    up: resolveToken("--color-emerald-600"),
    down: resolveToken("--color-rose-600"),
    degraded: resolveToken("--color-amber-400"),
    error: resolveToken("--color-rose-700"),
    unknown: resolveToken("--color-slate-400"),
    label: resolveToken("--color-slate-600"),
    headline: resolveToken("--color-slate-800"),
    font: resolveToken("--font-display", "font-family"),
};

// Thin white separator between slices (flat data viz; cartoon is chrome).
const SEP = { borderColor: "#ffffff", borderWidth: 2 };
const ANIMATE = !matchMedia("(prefers-reduced-motion: reduce)").matches;
// A segment narrower than this fraction of the total hides its inline
// count — the label would clip; the headline states both numbers anyway.
const LABEL_MIN_FRACTION = 0.12;

// Donut tooltip, flicker-proofed. A naive echarts pie tooltip over a thin
// ring repaints ~100×/s on mousemove. Every loop vector is closed here:
//   confine:true        → tooltip stays inside the chart box, never spills
//                          to <body> → no scrollbar → no window-resize
//                          feedback (bindResize listens on window resize).
//   pointer-events:none → cursor can't enter the tooltip → no
//                          enter-tooltip/leave-slice on/off oscillation.
//   transitionDuration:0→ no per-frame position lerp while the cursor moves.
//   emphasis.disabled   → hover doesn't grow the slice → no relayout
//                          repaint. (The bar keeps its inline labels and
//                          stays silent — a tooltip there is redundant.)
const TOOLTIP = {
    trigger: "item",
    formatter: "{b}: {c} ({d}%)",
    confine: true,
    enterable: false,
    transitionDuration: 0,
    backgroundColor: "#ffffff",
    borderColor: resolveToken("--color-slate-200"),
    borderWidth: 1,
    textStyle: { color: T.label, fontFamily: T.font },
    extraCssText:
        "pointer-events:none;border-radius:8px;box-shadow:0 2px 8px rgb(15 23 42 / 0.08)",
};

function donutOption(currentStatus) {
    const data = [
        { name: "up", value: currentStatus.up, itemStyle: { color: T.up, ...SEP } },
        { name: "down", value: currentStatus.down, itemStyle: { color: T.down, ...SEP } },
        { name: "degraded", value: currentStatus.degraded, itemStyle: { color: T.degraded, ...SEP } },
        { name: "error", value: currentStatus.error, itemStyle: { color: T.error, ...SEP } },
        { name: "unknown", value: currentStatus.unknown, itemStyle: { color: T.unknown, ...SEP } },
    ].filter(d => d.value > 0);
    return {
        animation: ANIMATE,
        textStyle: { fontFamily: T.font },
        tooltip: TOOLTIP,
        legend: { bottom: 0, type: "scroll", textStyle: { color: T.label, fontFamily: T.font } },
        series: [{
            type: "pie",
            radius: ["45%", "70%"],
            avoidLabelOverlap: false,
            itemStyle: { borderRadius: 2 },
            label: { show: false },
            emphasis: { disabled: true },
            data,
        }],
    };
}

function barOption(last24h) {
    const total = Math.max(0, last24h.checks_total);
    const up = Math.max(0, Math.min(last24h.checks_up, total));
    const notUp = total - up;
    const pct = total > 0 ? (up / total) * 100 : null;

    // Headline carries the numbers; the bar is the proportion at a glance.
    const title = {
        left: "center",
        top: "26%",
        text: pct === null ? "No checks in the last 24h" : `${pct.toFixed(2)}% up`,
        textStyle: { color: T.headline, fontFamily: T.font,
            fontSize: 22, fontWeight: 700 },
        subtext: pct === null ? ""
            : `${up.toLocaleString()} up · ${notUp.toLocaleString()} not up`,
        subtextStyle: { color: T.label, fontFamily: T.font, fontSize: 12 },
    };

    // Round only the outer ends; a zero segment hands its rounding to the
    // other so the bar is never half-flat. White border = thin gap between
    // segments, matching the donut's separator.
    const seg = (name, color, value, radius) => ({
        name,
        type: "bar",
        stack: "total",
        silent: true,
        barWidth: 26,
        itemStyle: { color, borderRadius: radius, borderColor: "#ffffff", borderWidth: 2 },
        data: [value],
        label: {
            show: value > 0 && value >= total * LABEL_MIN_FRACTION,
            position: "inside",
            color: "#ffffff",
            fontFamily: T.font,
            fontWeight: 600,
            formatter: value.toLocaleString(),
        },
    });

    return {
        animation: ANIMATE,
        textStyle: { fontFamily: T.font },
        title,
        // No hidden-axis gutter; bar spans the card under the headline.
        grid: { left: 16, right: 16, top: "62%", bottom: 24, containLabel: false },
        xAxis: { type: "value", show: false, max: total || 1 },
        yAxis: { type: "category", data: ["24h"], show: false },
        series: total === 0 ? [] : [
            seg("Up", T.up, up, notUp > 0 ? [6, 0, 0, 6] : 6),
            seg("Down / degraded / error", T.down, notUp, up > 0 ? [0, 6, 6, 0] : 6),
        ],
    };
}

const ENDPOINT = "/api/v1/dashboard/summary";

const slots = [
    { id: "status-donut", build: s => donutOption(s.current_status) },
    { id: "last24h-bar", build: s => barOption(s.last_24h) },
];

const mounted = [];
// Skip a poll-driven redraw while the cursor is over a chart: the every-5s
// notMerge re-set repaints the whole chart; landing that mid-interaction
// is jarring. The next htmx settle (≤5s) catches up. Tracked as a Set of
// hovered elements (not a shared bool) so a two-chart interleave can't
// clear it early; pointercancel resets so a touch-cancel can't latch
// polling off forever.
const hovered = new Set();

function applyAll(summary) {
    for (const m of mounted) {
        m.chart.setOption(m.build(summary), { notMerge: true });
    }
}

async function refresh() {
    if (mounted.length === 0 || hovered.size > 0) return;
    try {
        applyAll(await fetchJson(ENDPOINT));
    } catch (err) {
        console.warn("dashboard summary fetch failed", err);
    }
}

document.addEventListener("DOMContentLoaded", () => {
    for (const slot of slots) {
        const el = document.getElementById(slot.id);
        if (!el) continue;
        const chart = initChart(el);
        bindResize(chart);
        el.addEventListener("pointerenter", () => hovered.add(el));
        el.addEventListener("pointerleave", () => hovered.delete(el));
        el.addEventListener("pointercancel", () => hovered.delete(el));
        mounted.push({ chart, build: slot.build });
    }
    refresh();
});

// One fetch per dashboard-region settle drives every chart.
document.body.addEventListener("htmx:afterSettle", (e) => {
    if (e.detail?.target?.id === "dashboard-region") refresh();
});
