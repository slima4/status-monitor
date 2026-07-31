// One sparkline per declared step, each on its own scale: a step climbing
// 11 ms → 44 ms has quadrupled, and on a shared axis beside a 1200 ms `goto` it
// would draw as a flat line on the floor.
//
// Hand-drawn SVG rather than the library the other detail charts use, because a
// flow may declare 30 steps. `viewBox` and `currentColor` then leave resize and
// theme flips costing no JavaScript at all.

import { fetchJson, slidingWindow } from "./_init.js";

const LIVE_REGION = "detail-live-kpi";
const NS = "http://www.w3.org/2000/svg";
const VIEW_W = 160;
const VIEW_H = 40;
const PAD = 3;

// Ratio at which a step is called out as drifting rather than wobbling.
const DRIFT_AT = 1.5;
// Share of the range averaged at each end to compare them. One bucket alone
// makes a single spike look like a trend.
const END_WINDOW = 0.2;
// A run this far past the previous one never happened, so the line breaks
// instead of drawing through a gap the monitor cannot vouch for.
const GAP_FACTOR = 1.8;

const svgEl = (tag, attrs) => {
    const n = document.createElementNS(NS, tag);
    for (const k in attrs) n.setAttribute(k, attrs[k]);
    return n;
};

function fmtMs(ms) {
    if (ms >= 10000) return `${Math.round(ms / 1000)}s`;
    if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`;
    return `${Math.round(ms)}ms`;
}

function endMean(buckets, side) {
    const n = Math.max(1, Math.round(buckets.length * END_WINDOW));
    const slice = side === "head" ? buckets.slice(0, n) : buckets.slice(-n);
    return slice.reduce((sum, b) => sum + b.avg, 0) / slice.length;
}

// How much the step moved across the range. Null when there is too little to
// compare, so "no answer" never renders as "no change".
function driftRatio(buckets) {
    if (buckets.length < 4) return null;
    const head = endMean(buckets, "head");
    return head > 0 ? endMean(buckets, "tail") / head : null;
}

// Splits into runs of consecutive buckets so a stretch nothing reached breaks
// the line rather than drawing a straight segment across it.
function segments(buckets, bucketMs) {
    const out = [];
    let run = [];
    for (const b of buckets) {
        const prev = run[run.length - 1];
        if (prev && b.t - prev.t > bucketMs * GAP_FACTOR) {
            out.push(run);
            run = [];
        }
        run.push(b);
    }
    if (run.length) out.push(run);
    return out;
}

function project(buckets, tMin, tSpan, vMin, vSpan) {
    return buckets.map(b => {
        const x = PAD + ((b.t - tMin) / tSpan) * (VIEW_W - PAD * 2);
        const y = VIEW_H - PAD - ((b.avg - vMin) / vSpan) * (VIEW_H - PAD * 2);
        return [x, y];
    });
}

const line = pts => pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join("");

function sparkline(buckets, bucketMs) {
    const values = buckets.map(b => b.avg);
    const vMin = Math.min(...values);
    const vMax = Math.max(...values);
    // A flat series would divide by zero and draw on the top edge; centre it.
    const vSpan = vMax - vMin || Math.max(vMax, 1) * 2;
    const base = vMax === vMin ? vMin - vSpan / 2 : vMin;
    const tMin = buckets[0].t;
    const tSpan = buckets[buckets.length - 1].t - tMin || 1;

    const svg = svgEl("svg", {
        class: "flow-spark__svg",
        viewBox: `0 0 ${VIEW_W} ${VIEW_H}`,
        preserveAspectRatio: "none",
        "aria-hidden": "true",
    });

    for (const run of segments(buckets, bucketMs)) {
        const pts = project(run, tMin, tSpan, base, vSpan);
        if (pts.length > 1) {
            const [x0] = pts[0];
            const [xn] = pts[pts.length - 1];
            svg.appendChild(svgEl("path", {
                class: "flow-spark__area",
                d: `${line(pts)}L${xn.toFixed(1)},${VIEW_H}L${x0.toFixed(1)},${VIEW_H}Z`,
            }));
        }
        svg.appendChild(svgEl("path", {
            class: "flow-spark__line",
            // A lone point has no line to draw, so give it a dot's worth of one.
            d: pts.length > 1 ? line(pts) : `${line(pts)}l0.01,0`,
        }));
    }

    const last = project(buckets.slice(-1), tMin, tSpan, base, vSpan)[0];
    svg.appendChild(svgEl("circle", { class: "flow-spark__tip", cx: last[0], cy: last[1], r: 2 }));
    return svg;
}

function cell(step, bucketMs, tz) {
    const buckets = step.buckets ?? [];
    const name = `${step.step + 1} ${step.op}`;
    const ratio = driftRatio(buckets);
    const drifting = ratio !== null && ratio >= DRIFT_AT;

    const root = document.createElement("li");
    root.className = `flow-spark${drifting ? " flow-spark--drift" : ""}`;

    const head = document.createElement("p");
    head.className = "flow-spark__name";
    head.textContent = name;
    root.appendChild(head);

    if (!buckets.length) {
        const none = document.createElement("p");
        none.className = "flow-spark__none";
        none.textContent = "never reached";
        root.appendChild(none);
        return root;
    }

    root.appendChild(sparkline(buckets, bucketMs));

    const foot = document.createElement("p");
    foot.className = "flow-spark__foot";

    const value = document.createElement("span");
    value.className = "flow-spark__value";
    value.textContent = fmtMs(buckets[buckets.length - 1].avg);
    foot.appendChild(value);

    const delta = document.createElement("span");
    delta.className = `flow-spark__delta${drifting ? " flow-spark__delta--up" : ""}`;
    if (ratio === null) {
        delta.textContent = "—";
        delta.title = "not enough history in this range to compare";
    } else {
        const pct = Math.round((ratio - 1) * 100);
        delta.textContent = `${pct > 0 ? "+" : ""}${pct}%`;
        delta.title = "change from the start of the range to now";
    }
    foot.appendChild(delta);
    root.appendChild(foot);

    // Reading the value at a point matters more here than a full tooltip layer:
    // the question is "how slow was it then", and the header already has room.
    const readout = (b) => {
        value.textContent = fmtMs(b.avg);
        head.textContent = `${name} · ${tz.format(new Date(b.t))}`;
    };
    const reset = () => {
        value.textContent = fmtMs(buckets[buckets.length - 1].avg);
        head.textContent = name;
    };
    root.addEventListener("pointermove", (e) => {
        const box = root.getBoundingClientRect();
        const at = (e.clientX - box.left) / box.width;
        readout(buckets[Math.min(buckets.length - 1, Math.max(0, Math.round(at * (buckets.length - 1))))]);
    });
    root.addEventListener("pointerleave", reset);

    const label = `${name}: ${fmtMs(Math.min(...buckets.map(b => b.avg)))} to ` +
        `${fmtMs(Math.max(...buckets.map(b => b.avg)))}, now ${fmtMs(buckets[buckets.length - 1].avg)}`;
    root.setAttribute("role", "img");
    root.setAttribute("aria-label", label);
    return root;
}

function start() {
    const host = document.querySelector("#flow-steps[data-endpoint]");
    if (!host) return;

    const query = slidingWindow(host.dataset.endpoint);
    const tz = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

    async function refresh() {
        let data;
        try {
            data = await fetchJson(query().url);
        } catch (err) {
            console.warn("flow step trend load failed", err);
            return;
        }
        const steps = data.steps ?? [];
        host.textContent = "";
        if (!steps.length) {
            const empty = document.createElement("p");
            empty.className = "flow-spark-empty";
            empty.textContent = "No step timings in this range yet.";
            host.appendChild(empty);
            return;
        }
        const bucketMs = (data.bucket_seconds || 60) * 1000;
        const grid = document.createElement("ul");
        grid.className = "flow-spark-grid";
        for (const step of steps) grid.appendChild(cell(step, bucketMs, tz));
        host.appendChild(grid);
    }

    refresh();
    document.body.addEventListener("htmx:afterSettle", (e) => {
        if (e.detail?.target?.id === LIVE_REGION) refresh();
    });
}

if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
} else { start(); }
