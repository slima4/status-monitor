import { timeXAxis, msChartBase } from "./_init.js";

const PHASES = [
    { key: "dns", name: "DNS lookup", color: "#a78bfa" },
    { key: "connect", name: "Connect", color: "#60a5fa" },
    { key: "tls", name: "TLS handshake", color: "#34d399" },
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
    return { ...msChartBase(), xAxis: { ...timeXAxis(from, to), boundaryGap: false }, series };
}

// Weighted by samples: buckets hold unequal probe counts, so a flat mean of the
// bucket means would let a one-probe bucket outvote a full one.
function phaseMeans(buckets) {
    const totals = { dns: 0, connect: 0, tls: 0, ttfb: 0, app: 0 };
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

// Multi-region breakdown: one stacked bar per region. Regions with no samples in
// the range carry no phases to draw, so they get no bar.
export function buildBreakdownByRegionOption(regions) {
    const rows = regions
        .map(r => ({ id: r.region, label: r.label || r.region, means: phaseMeans(r.buckets ?? []) }))
        .filter(r => r.means);
    // Ascending: a category axis draws index 0 at the bottom, so the slowest
    // region ends up the top bar.
    const total = r => PHASES.reduce((sum, { key }) => sum + r.means[key], 0);
    rows.sort((a, b) => total(a) - total(b));

    const series = PHASES.map(({ key, name, color }) => ({
        name,
        type: "bar",
        stack: "lat",
        itemStyle: { color },
        // `regionId` rides along so a click resolves to a region without
        // reversing the sort or matching on the display label.
        data: rows.map(r => ({ value: r.means[key], regionId: r.id })),
    }));
    const base = msChartBase();
    return {
        ...base,
        tooltip: { ...base.tooltip, trigger: "axis", axisPointer: { type: "shadow" } },
        // Region labels run long ("🇸🇬 apac-sg (Singapore)"); let the axis size itself.
        grid: { ...base.grid, left: 8, containLabel: true },
        xAxis: { type: "value", axisLabel: { formatter: "{value} ms" } },
        yAxis: { type: "category", data: rows.map(r => r.label) },
        series,
    };
}
