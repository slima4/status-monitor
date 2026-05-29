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
