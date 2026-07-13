// Shared ECharts helpers. Stays pure (no DOM globals beyond `echarts`) so
// SvelteKit can import it verbatim during a future migration.

export function initChart(el) {
    return echarts.init(el, null, { renderer: "canvas" });
}

// Replaces a chart container with a centered placeholder string. Use
// when the fetched payload has no items so the user sees a clear "no
// data yet" instead of an empty axes-only chart they might read as
// broken. `msg` is plain text — caller is responsible for escaping.
export function renderEmptyChart(el, msg) {
    el.innerHTML = "";
    const wrap = document.createElement("div");
    wrap.className = "flex h-full items-center justify-center font-mono text-xs text-quiet";
    wrap.textContent = msg;
    el.appendChild(wrap);
}

// Resolve a Tailwind theme token (`--color-*`, `--font-*`) to a concrete
// value via a throwaway probe so charts share the CSS palette instead of
// hardcoding hex. Tokens are oklch(); a raw oklch() canvas fillStyle fails
// silently on older engines, so let the browser hand back rgb(). A missing
// / mistyped token doesn't error — the invalid `var()` makes the probe
// inherit the body value, so a typo ships a body-coloured chart, not a
// crash. Call once per token (values are static; a theme swap reloads).
export function resolveToken(name, prop = "color") {
    const probe = document.createElement("span");
    probe.style.cssText = `position:absolute;visibility:hidden;${prop}:var(${name})`;
    document.body.appendChild(probe);
    const value = getComputedStyle(probe)[prop];
    probe.remove();
    return value;
}

// ECharts' hover emphasis parses colours numerically and only groks rgb/hex;
// a token that resolves to oklch comes back from getComputedStyle as oklab(),
// which it can't parse. Paint a pixel and read it back to flatten to sRGB.
let colorCtx = null;
function toRgb(value) {
    if (!colorCtx) colorCtx = document.createElement("canvas").getContext("2d");
    colorCtx.fillStyle = value;
    colorCtx.fillRect(0, 0, 1, 1);
    const [r, g, b] = colorCtx.getImageData(0, 0, 1, 1).data;
    return `rgb(${r}, ${g}, ${b})`;
}

const tokenCache = new Map();
export function resolveTokenCached(name, prop = "color") {
    const key = `${prop} ${name}`;
    if (!tokenCache.has(key)) {
        const raw = resolveToken(name, prop);
        tokenCache.set(key, prop === "color" ? toRgb(raw) : raw);
    }
    return tokenCache.get(key);
}

export async function fetchJson(url) {
    const res = await fetch(url, { headers: { Accept: "application/json" } });
    if (!res.ok) throw new Error(`${url} → ${res.status}`);
    return res.json();
}

// Range-aware x-axis tick formatter for a `type:"time"` axis. ≤24h shows
// clock time; multi-day shows the date, plus clock time on intra-day ticks
// so a day-boundary tick reads "05-23" not "05-23 00:00". Renders in the
// browser's local timezone, matching the rest of the UI (localtime.js).
const DAY_MS = 86400000;
export function timeAxisFormatter(from, to) {
    const span = +to - +from;
    const p = (n) => String(n).padStart(2, "0");
    return (value) => {
        const d = new Date(value);
        if (span <= DAY_MS) return `${p(d.getHours())}:${p(d.getMinutes())}`;
        if (span <= 7 * DAY_MS && (d.getHours() || d.getMinutes())) return `${p(d.getHours())}:${p(d.getMinutes())}`;
        return `${p(d.getMonth() + 1)}-${p(d.getDate())}`;
    };
}

// Shared time x-axis pinned to [from, to] so the axis spans the selected
// range even when data covers only part of it (a 30-min monitor on a 1h
// range shows data in the matching slice, not stretched full-width). Both
// detail charts render the same window, so they share one axis config.
export function timeXAxis(from, to) {
    return {
        type: "time",
        min: +from,
        max: +to,
        axisLabel: { formatter: timeAxisFormatter(from, to), hideOverlap: true },
    };
}

// The page's mono stack, resolved once — axis/legend/tooltip text renders in
// the same face as the rest of the console UI instead of the ECharts default.
let monoStack = null;
function chartFont() {
    if (monoStack === null) monoStack = resolveToken("--font-mono", "font-family");
    return monoStack;
}

// Shared config for the two ms-valued detail-page charts: unit-suffixed
// tooltip/y-axis and grid padding sized for the "{value} ms" labels. Callers
// spread this and add their own xAxis/series.
export function msChartBase() {
    const fontFamily = chartFont();
    return {
        textStyle: { fontFamily },
        tooltip: {
            trigger: "axis",
            valueFormatter: v => v == null ? "—" : `${v} ms`,
            textStyle: { fontFamily },
        },
        // `grid.bottom` reserves one legend row. A wrapped second row would grow up
        // into the axis labels, so the legend pages sideways instead of wrapping.
        legend: { type: "scroll", bottom: 0, textStyle: { fontFamily } },
        grid: { left: 60, right: 20, top: 20, bottom: 40 },
        yAxis: { type: "value", axisLabel: { formatter: "{value} ms" } },
    };
}

// Re-derives a [now-span, now] window on each call so the chart tracks now
// instead of freezing at the page-load window (which would exclude checks
// that ran after load). Span and page size come from the initial endpoint.
export function slidingWindow(endpoint) {
    const u = new URL(endpoint, window.location.origin);
    const f = u.searchParams.get("from");
    const t = u.searchParams.get("to");
    const spanMs = f && t ? +new Date(t) - +new Date(f) : 24 * 3600 * 1000;
    const limit = u.searchParams.get("limit");
    const region = u.searchParams.get("region");
    const base = u.origin + u.pathname;
    return () => {
        const to = new Date();
        const from = new Date(+to - spanMs);
        const params = new URLSearchParams({ from: from.toISOString(), to: to.toISOString() });
        if (limit) params.set("limit", limit);
        if (region) params.set("region", region);
        return { url: `${base}?${params}`, from, to };
    };
}
