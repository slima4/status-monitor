import { timeXAxis, msChartBase, resolveTokenCached } from "./_init.js";

const PHASES = [
    { key: "dns", name: "DNS lookup", token: "--color-chart-phase-dns" },
    { key: "connect", name: "Connect", token: "--color-chart-phase-connect" },
    { key: "tls", name: "TLS handshake", token: "--color-chart-phase-tls" },
    { key: "ttfb", name: "Server response", token: "--color-chart-phase-ttfb" },
    { key: "app", name: "Processing", token: "--color-chart-phase-app" },
];

// Hairline in the surface colour between stacked bands: the hues are ~ΔE 10 apart
// under deuteranopia, so the edge carries separation the colour alone can't.
const SEAM = 1;
const seamColor = () => resolveTokenCached("--theme-surface-elev");

// Server-aggregated buckets carry mean per-phase timings plus the mean total
// (`avg`). Processing = total − measured phases (≥ 0); for check kinds without
// phase timing (tcp/dns) the phases are 0 so the whole band is "Processing".
function appPhase(b) {
    return Math.max(0, b.avg - (b.dns + b.connect + b.tls + b.ttfb));
}

export function buildBreakdownOption(buckets, from, to) {
    const seam = seamColor();
    const series = PHASES.map(({ key, name, token }) => ({
        name,
        type: "line",
        stack: "lat",
        areaStyle: { opacity: 0.85 },
        showSymbol: false,
        lineStyle: { width: SEAM, color: seam },
        itemStyle: { color: resolveTokenCached(token) },
        // [timestamp, value] pairs; `t` is unix-millis.
        data: buckets.map(b => [b.t, key === "app" ? appPhase(b) : b[key]]),
    }));
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

// Multi-region breakdown: one stacked bar per region. Regions with no samples in
// the range carry no phases to draw, so they get no bar.
export function buildBreakdownByRegionOption(regions) {
    const rows = regions
        .map(r => ({ id: r.region, label: r.label || r.region, means: phaseMeans(r.buckets ?? []) }))
        .filter(r => r.means)
        .map(r => ({ ...r, total: PHASES.reduce((sum, { key }) => sum + r.means[key], 0) }));
    // Ascending: a category axis draws index 0 at the bottom, so the slowest
    // region ends up the top bar.
    rows.sort((a, b) => a.total - b.total);

    const seam = seamColor();
    const series = PHASES.map(({ key, name, token }) => ({
        name,
        type: "bar",
        stack: "lat",
        itemStyle: { color: resolveTokenCached(token), borderColor: seam },
        // `regionId` rides along so a click resolves to a region without
        // reversing the sort or matching on the display label. The border is
        // inset, so a phase too thin to spare it renders without one.
        data: rows.map(r => ({
            value: r.means[key],
            regionId: r.id,
            itemStyle: { borderWidth: r.total > 0 && r.means[key] / r.total > 0.01 ? SEAM : 0 },
        })),
    }));
    const base = msChartBase();
    // `containLabel` is ECharts-6 shorthand for `outerBoundsMode: "same"`, which
    // fits the label inside `grid.left` instead of growing it — a long region
    // clips. Reserve the widest label up front, and let the axis grow past that
    // if it measures wider (an emoji outruns a monospace advance).
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
