import { timeXAxis, msChartBase } from "./_init.js";

const QUANTILES = [
    { name: "p50", key: "p50", color: "#0ea5e9" },
    { name: "p95", key: "p95", color: "#6366f1" },
    { name: "p99", key: "p99", color: "#dc2626" },
];

// `buckets` are server-aggregated (one slice per ~range/60), each carrying
// p50/p95/p99 in ms. The server omits slices with no samples, so a no-data
// span renders as a straight segment between its neighbours, not a dip to 0.
export function buildLatencyOption(buckets, from, to) {
    const series = QUANTILES.map(({ name, key, color }) => ({
        name,
        type: "line",
        smooth: true,
        showSymbol: false,
        itemStyle: { color },
        lineStyle: { color, width: 2 },
        // Time axis needs [timestamp, value] pairs; `t` is unix-millis.
        data: buckets.map(b => [b.t, b[key]]),
    }));
    return { ...msChartBase(), xAxis: timeXAxis(from, to), series };
}

// Color cycle for the per-region overlay; wraps past its length.
const REGION_COLORS = [
    "#0ea5e9", "#6366f1", "#dc2626", "#16a34a",
    "#d97706", "#7c3aed", "#0891b2", "#db2777",
];

// Multi-region overlay: one p95 line per region so regions compare directly.
// `regions` is `[{ region, buckets }]`; p50/p99 are dropped — comparing one
// metric across regions is the whole point, three quantiles × N regions is noise.
export function buildLatencyOverlayOption(regions, from, to) {
    const series = regions.map((r, i) => {
        const color = REGION_COLORS[i % REGION_COLORS.length];
        return {
            name: r.region,
            type: "line",
            smooth: true,
            showSymbol: false,
            itemStyle: { color },
            lineStyle: { color, width: 2 },
            data: (r.buckets ?? []).map(b => [b.t, b.p95]),
        };
    });
    return { ...msChartBase(), xAxis: timeXAxis(from, to), series };
}
