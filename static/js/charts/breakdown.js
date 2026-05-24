import { mountChartFromFetch, wireChartElements } from "./_init.js";

const PHASES = [
    { key: "dns_ms", name: "DNS", color: "#a78bfa" },
    { key: "connect_ms", name: "Connect", color: "#60a5fa" },
    { key: "tls_ms", name: "TLS", color: "#34d399" },
    { key: "ttfb_ms", name: "Server response", color: "#fbbf24" },
    { key: "app_ms", name: "Processing", color: "#fb7185" },
];

// API exposes dns/connect/tls/ttfb; remaining time = server processing.
function appPhase(r) {
    const sum = (r.dns_ms || 0) + (r.connect_ms || 0) + (r.tls_ms || 0) + (r.ttfb_ms || 0);
    return Math.max(0, (r.duration_ms || 0) - sum);
}

function buildOption(items) {
    const sorted = [...items].sort((a, b) => +new Date(a.timestamp) - +new Date(b.timestamp));
    const enriched = sorted.map(r => ({ ...r, app_ms: appPhase(r) }));
    const xs = sorted.map(r => new Date(r.timestamp).toISOString());
    const series = PHASES.map(({ key, name, color }) => ({
        name,
        type: "line",
        stack: "lat",
        areaStyle: { opacity: 0.85 },
        showSymbol: false,
        lineStyle: { width: 0 },
        itemStyle: { color },
        data: enriched.map(r => r[key] || 0),
    }));
    return {
        tooltip: { trigger: "axis" },
        legend: { bottom: 0 },
        grid: { left: 50, right: 20, top: 20, bottom: 40 },
        xAxis: { type: "category", data: xs, boundaryGap: false, show: false },
        yAxis: { type: "value", name: "ms" },
        series,
    };
}

function initBreakdownChart(el) {
    return mountChartFromFetch(
        el,
        el.dataset.endpoint,
        (chart, items) => chart.setOption(buildOption(items), { notMerge: true }),
        "No data in this range yet.",
    );
}

wireChartElements("#breakdown-chart[data-endpoint]", initBreakdownChart);
