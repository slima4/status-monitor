import { initChart, fetchJson, bindResize } from "./_init.js";

const STATUS_COLORS = {
    up: "#16a34a",
    down: "#dc2626",
    degraded: "#d97706",
    error: "#e11d48",
    unknown: "#94a3b8",
};

function donutOption(currentStatus) {
    const data = [
        { name: "up", value: currentStatus.up, itemStyle: { color: STATUS_COLORS.up } },
        { name: "down", value: currentStatus.down, itemStyle: { color: STATUS_COLORS.down } },
        { name: "degraded", value: currentStatus.degraded, itemStyle: { color: STATUS_COLORS.degraded } },
        { name: "error", value: currentStatus.error, itemStyle: { color: STATUS_COLORS.error } },
        { name: "unknown", value: currentStatus.unknown, itemStyle: { color: STATUS_COLORS.unknown } },
    ].filter(d => d.value > 0);
    return {
        tooltip: { trigger: "item", formatter: "{b}: {c} ({d}%)" },
        legend: { bottom: 0, type: "scroll" },
        series: [{
            type: "pie",
            radius: ["45%", "70%"],
            avoidLabelOverlap: false,
            label: { show: false },
            data,
        }],
    };
}

function barOption(last24h) {
    const total = Math.max(0, last24h.checks_total);
    const up = Math.max(0, Math.min(last24h.checks_up, total));
    const notUp = total - up;
    return {
        tooltip: { trigger: "axis", axisPointer: { type: "shadow" } },
        grid: { left: 60, right: 20, top: 20, bottom: 30 },
        xAxis: { type: "value", show: false, max: total || 1 },
        yAxis: { type: "category", data: ["24h"], show: false },
        series: [
            {
                name: "Up",
                type: "bar",
                stack: "total",
                itemStyle: { color: STATUS_COLORS.up },
                data: [up],
                label: { show: up > 0, position: "inside", formatter: `${up.toLocaleString()} up` },
            },
            {
                name: "Down / degraded / error",
                type: "bar",
                stack: "total",
                itemStyle: { color: STATUS_COLORS.down },
                data: [notUp],
                label: { show: notUp > 0, position: "inside", formatter: `${notUp.toLocaleString()} not up` },
            },
        ],
    };
}

const ENDPOINT = "/api/v1/dashboard/summary";

const slots = [
    { id: "status-donut", build: s => donutOption(s.current_status) },
    { id: "last24h-bar", build: s => barOption(s.last_24h) },
];

const mounted = [];

function applyAll(summary) {
    for (const m of mounted) {
        m.chart.setOption(m.build(summary), { notMerge: true });
    }
}

async function refresh() {
    if (mounted.length === 0) return;
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
        mounted.push({ chart, build: slot.build });
    }
    refresh();
});

// One fetch per dashboard-region settle drives every chart.
document.body.addEventListener("htmx:afterSettle", (e) => {
    if (e.detail?.target?.id === "dashboard-region") refresh();
});
