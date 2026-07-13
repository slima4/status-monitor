import { timeXAxis, msChartBase, resolveTokenCached } from "./_init.js";

const QUANTILES = [
    { name: "p50", key: "p50", token: "--color-chart-p50" },
    { name: "p95", key: "p95", token: "--color-chart-p95" },
    { name: "p99", key: "p99", token: "--color-chart-p99" },
];

// `buckets` are server-aggregated (one slice per ~range/60), each carrying
// p50/p95/p99 in ms. The server omits slices with no samples, so a no-data
// span renders as a straight segment between its neighbours, not a dip to 0.
export function buildLatencyOption(buckets, from, to) {
    const series = QUANTILES.map(({ name, key, token }) => {
        const color = resolveTokenCached(token);
        return {
            name,
            type: "line",
            smooth: true,
            showSymbol: false,
            itemStyle: { color },
            lineStyle: { color, width: 2 },
            // Time axis needs [timestamp, value] pairs; `t` is unix-millis.
            data: buckets.map(b => [b.t, b[key]]),
        };
    });
    return { ...msChartBase(), xAxis: timeXAxis(from, to), series };
}

const REGION_TOKENS = [
    "--color-chart-1", "--color-chart-2", "--color-chart-3", "--color-chart-4",
    "--color-chart-5", "--color-chart-6", "--color-chart-7", "--color-chart-8",
];

// Multi-region overlay: one median line per region. `regions` is
// `[{ region, label, buckets }]`; a bucket holds too few probes for its tail
// quantiles to be stable, so the tail stays on the single-region chart.
export function buildLatencyOverlayOption(regions, from, to) {
    const series = regions.map((r, i) => {
        const color = resolveTokenCached(REGION_TOKENS[i % REGION_TOKENS.length]);
        return {
            name: r.label || r.region,
            type: "line",
            smooth: true,
            showSymbol: false,
            itemStyle: { color },
            lineStyle: { color, width: 2 },
            data: (r.buckets ?? []).map(b => [b.t, b.p50]),
        };
    });
    return { ...msChartBase(), xAxis: timeXAxis(from, to), series };
}
