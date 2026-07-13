import { timeXAxis, msChartBase, resolveTokenCached } from "./_init.js";

// Names stay short so the legend never needs a second row.
const PHASES = [
    { key: "dns", name: "DNS", token: "--color-chart-phase-dns" },
    { key: "connect", name: "connect", token: "--color-chart-phase-connect" },
    { key: "tls", name: "TLS", token: "--color-chart-phase-tls" },
    { key: "ttfb", name: "TTFB", token: "--color-chart-phase-ttfb" },
    { key: "app", name: "processing", token: "--color-chart-phase-app" },
];

// Server-aggregated buckets carry mean per-phase timings plus the mean total
// (`avg`). Processing = total − measured phases (≥ 0); for check kinds without
// phase timing (tcp/dns) the phases are 0 so the whole band is "Processing".
function appPhase(b) {
    return Math.max(0, b.avg - (b.dns + b.connect + b.tls + b.ttfb));
}

export function buildBreakdownOption(buckets, from, to) {
    const series = PHASES.map(({ key, name, token }) => {
        const color = resolveTokenCached(token);
        return {
            name,
            type: "line",
            stack: "lat",
            areaStyle: { color },
            showSymbol: false,
            lineStyle: { width: 0 },
            itemStyle: { color },
            // [timestamp, value] pairs; `t` is unix-millis.
            data: buckets.map(b => [b.t, key === "app" ? appPhase(b) : b[key]]),
        };
    });
    return { ...msChartBase(), xAxis: { ...timeXAxis(from, to), boundaryGap: false }, series };
}

// Weighted by samples: buckets hold unequal probe counts, so a flat mean of the
// bucket means would let a one-probe bucket outvote a full one.
function phaseMeans(buckets) {
    const totals = Object.fromEntries(PHASES.map(({ key }) => [key, 0]));
    let n = 0;
    for (const b of buckets) {
        const w = b.samples ?? 0;
        if (w <= 0) continue;
        n += w;
        for (const { key } of PHASES) {
            totals[key] += (key === "app" ? appPhase(b) : b[key]) * w;
        }
    }
    if (n === 0) return null;
    return Object.fromEntries(PHASES.map(({ key }) => [key, Math.round(totals[key] / n)]));
}

export function buildBreakdownByRegionOption(regions) {
    const rows = regions
        .map(r => ({ id: r.region, label: r.label || r.region, means: phaseMeans(r.buckets ?? []) }))
        .filter(r => r.means)
        .map(r => ({ ...r, total: PHASES.reduce((sum, { key }) => sum + r.means[key], 0) }));
    // A category axis draws index 0 at the bottom, so ascending puts the slowest on top.
    rows.sort((a, b) => a.total - b.total);

    const series = PHASES.map(({ key, name, token }) => ({
        name,
        type: "bar",
        stack: "lat",
        itemStyle: { color: resolveTokenCached(token) },
        data: rows.map(r => ({ value: r.means[key], regionId: r.id })),
    }));
    const base = msChartBase();
    // Not `containLabel`: in ECharts 6 it fits the label into `grid.left` rather
    // than growing it, and long region names clip.
    const widest = rows.reduce((w, r) => Math.max(w, r.label.length), 0);
    return {
        ...base,
        tooltip: { ...base.tooltip, trigger: "axis", axisPointer: { type: "shadow" } },
        grid: {
            ...base.grid,
            left: Math.min(24 + widest * 8, 220),
            outerBoundsContain: "axisLabel",
        },
        xAxis: { type: "value", axisLabel: { formatter: "{value} ms" } },
        yAxis: { type: "category", data: rows.map(r => r.label) },
        series,
    };
}
