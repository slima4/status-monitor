import { quantile, timeBuckets } from "./_init.js";

const BUCKETS = 60;
const QUANTILES = [
    { name: "p50", q: 0.5, color: "#0ea5e9" },
    { name: "p95", q: 0.95, color: "#6366f1" },
    { name: "p99", q: 0.99, color: "#dc2626" },
];

export function buildLatencyOption(items, from, to) {
    const buckets = timeBuckets(items, from, to, BUCKETS, r => r.duration_ms);
    const sortedBuckets = buckets.map(b => ({
        t: b.t,
        sorted: [...b.values].sort((a, b) => a - b),
    }));
    const series = QUANTILES.map(({ name, q, color }) => ({
        name,
        type: "line",
        smooth: true,
        showSymbol: false,
        connectNulls: false,
        itemStyle: { color },
        lineStyle: { color, width: 2 },
        // Time-axis series needs [timestamp, value] pairs — a bare value
        // array works only for a category axis. Without the timestamp
        // every point lands at x=0 and the line is invisible.
        data: sortedBuckets.map(b => [
            b.t,
            b.sorted.length === 0 ? null : Math.round(quantile(b.sorted, q)),
        ]),
    }));
    return {
        tooltip: { trigger: "axis" },
        legend: { bottom: 0 },
        grid: { left: 50, right: 20, top: 20, bottom: 40 },
        xAxis: { type: "time", min: from, max: to },
        yAxis: { type: "value", name: "ms", axisLabel: { formatter: "{value}" } },
        series,
    };
}
