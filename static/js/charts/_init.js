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
    wrap.className = "flex h-full items-center justify-center text-sm text-slate-500";
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

export async function fetchJson(url) {
    const res = await fetch(url, { headers: { Accept: "application/json" } });
    if (!res.ok) throw new Error(`${url} → ${res.status}`);
    return res.json();
}

// Page handlers return either `{items, total, ...}` (paged) or a bare array
// (legacy). Normalise both so chart code can stay agnostic.
export function unwrapItems(json) {
    return Array.isArray(json) ? json : (json?.items ?? []);
}

// Returns numeric quantile from a sorted ascending array. `q` in [0, 1].
export function quantile(sorted, q) {
    if (sorted.length === 0) return 0;
    const pos = (sorted.length - 1) * q;
    const lo = Math.floor(pos);
    const hi = Math.ceil(pos);
    if (lo === hi) return sorted[lo];
    return sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo);
}

// Bin timestamps into N equal time buckets between [from, to). Returns
// an array of { t, values } where `values` is the numeric samples in that
// bucket; callers compute their own aggregate (mean / quantile / count).
export function timeBuckets(items, from, to, bucketCount, valueFn) {
    const fromMs = +from;
    const toMs = +to;
    const span = Math.max(1, toMs - fromMs);
    const buckets = Array.from({ length: bucketCount }, (_, i) => ({
        t: new Date(fromMs + (i + 0.5) * (span / bucketCount)),
        values: [],
    }));
    for (const item of items) {
        const ms = +new Date(item.timestamp);
        if (ms < fromMs || ms >= toMs) continue;
        const idx = Math.min(
            bucketCount - 1,
            Math.floor(((ms - fromMs) / span) * bucketCount),
        );
        const v = valueFn(item);
        if (Number.isFinite(v)) buckets[idx].values.push(v);
    }
    return buckets;
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
    const base = u.origin + u.pathname;
    return () => {
        const to = new Date();
        const from = new Date(+to - spanMs);
        const params = new URLSearchParams({ from: from.toISOString(), to: to.toISOString() });
        if (limit) params.set("limit", limit);
        return { url: `${base}?${params}`, from, to };
    };
}
