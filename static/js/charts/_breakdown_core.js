import { timeXAxis } from "./_init.js";

const PHASES = [
    { key: "dns", name: "DNS", color: "#a78bfa" },
    { key: "connect", name: "Connect", color: "#60a5fa" },
    { key: "tls", name: "TLS", color: "#34d399" },
    { key: "ttfb", name: "Server response", color: "#fbbf24" },
    { key: "app", name: "Processing", color: "#fb7185" },
];

// Server-aggregated buckets carry mean per-phase timings plus the mean total
// (`avg`). Processing = total − measured phases (≥ 0); for check kinds without
// phase timing (tcp/dns) the phases are 0 so the whole band is "Processing".
function appPhase(b) {
    return Math.max(0, b.avg - (b.dns + b.connect + b.tls + b.ttfb));
}

export function buildBreakdownOption(buckets, from, to) {
    const series = PHASES.map(({ key, name, color }) => ({
        name,
        type: "line",
        stack: "lat",
        areaStyle: { opacity: 0.85 },
        showSymbol: false,
        lineStyle: { width: 0 },
        itemStyle: { color },
        // [timestamp, value] pairs; `t` is unix-millis.
        data: buckets.map(b => [b.t, key === "app" ? appPhase(b) : b[key]]),
    }));
    return {
        tooltip: { trigger: "axis" },
        legend: { bottom: 0 },
        grid: { left: 50, right: 20, top: 20, bottom: 40 },
        xAxis: { ...timeXAxis(from, to), boundaryGap: false },
        yAxis: { type: "value", name: "ms" },
        series,
    };
}
