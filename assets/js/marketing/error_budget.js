// Live SLO -> error-budget + burn-rate calculator. The page ships a fully
// server-rendered default state, so this only upgrades it to recompute on
// input. Math mirrors the Rust side so hydration never changes the numbers.
function humanDuration(secs) {
  if (!(secs > 0)) return "0s";
  if (secs < 1) return `${Math.round(secs * 1000)}ms`;
  if (secs < 10) {
    const v = Math.round(secs * 10) / 10;
    return Number.isInteger(v) ? `${v}s` : `${v.toFixed(1)}s`;
  }
  let total = Math.round(secs);
  const d = Math.floor(total / 86_400);
  total %= 86_400;
  const h = Math.floor(total / 3_600);
  total %= 3_600;
  const m = Math.floor(total / 60);
  const s = total % 60;
  const parts = [];
  if (d) parts.push(`${d}d`);
  if (h) parts.push(`${h}h`);
  if (m) parts.push(`${m}m`);
  if (s) parts.push(`${s}s`);
  return parts.join(" ");
}

function downtime(pct, windowSecs) {
  return windowSecs * (1 - pct / 100);
}

function burnRate(measured, slo) {
  const allowed = 1 - slo / 100;
  if (allowed <= 0) return measured >= 100 ? 0 : Infinity;
  return Math.max((1 - measured / 100) / allowed, 0);
}

function formatMultiple(rate) {
  if (!Number.isFinite(rate)) return "∞×";
  return `${parseFloat(rate.toFixed(2))}×`;
}

function budgetResult(slo, measured, windowSecs) {
  const total = downtime(slo, windowSecs);
  const used = downtime(measured, windowSecs);
  const remaining = Math.max(total - used, 0);
  const rate = burnRate(measured, slo);

  let verdict;
  let state;
  if (measured >= 100) {
    verdict = "on track, no downtime and full budget intact";
    state = "ok";
  } else if (!Number.isFinite(rate)) {
    verdict = "over budget, a 100% SLO leaves no room for any downtime";
    state = "warn";
  } else if (rate < 1) {
    verdict = `under budget, spending at ${formatMultiple(rate)} the allowed rate`;
    state = "ok";
  } else if (Math.abs(rate - 1) < 0.001) {
    verdict = "on budget, spends the window's allowance exactly";
    state = "on";
  } else {
    verdict = `over budget, the allowance runs out in ${humanDuration(windowSecs / rate)}`;
    state = "warn";
  }

  return {
    total: humanDuration(total),
    used: humanDuration(used),
    remaining: humanDuration(remaining),
    burn: formatMultiple(rate),
    verdict,
    state,
  };
}

// Chart geometry — kept byte-identical to the Rust burn_chart() so the first
// client redraw lands on the server-rendered pixels.
const CHART_X0 = 40;
const CHART_X1 = 568;
const CHART_Y0 = 16;
const CHART_Y1 = 190;

function chartX(t) {
  return CHART_X0 + t * (CHART_X1 - CHART_X0);
}

function chartY(frac) {
  return CHART_Y0 + (1 - frac) * (CHART_Y1 - CHART_Y0);
}

function point(x, y) {
  return `${x.toFixed(1)},${y.toFixed(1)}`;
}

function buildChart(slo, measured, windowSecs) {
  const rate = burnRate(measured, slo);
  const over = !Number.isFinite(rate) || rate > 1;
  if (over) {
    const tStar = Number.isFinite(rate) ? 1 / rate : 0;
    const crossX = chartX(tStar);
    return {
      linePoints: `${point(chartX(0), chartY(1))} ${point(crossX, chartY(0))} ${point(chartX(1), chartY(0))}`,
      dotX: crossX.toFixed(1),
      dotY: chartY(0).toFixed(1),
      labelX: crossX.toFixed(1),
      labelY: (chartY(0) - 8).toFixed(1),
      labelAnchor: "middle",
      labelText: Number.isFinite(rate) ? `gone in ${humanDuration(windowSecs / rate)}` : "no budget",
      state: "warn",
    };
  }
  const endFrac = 1 - rate;
  const endY = chartY(endFrac);
  return {
    linePoints: `${point(chartX(0), chartY(1))} ${point(chartX(1), endY)}`,
    dotX: CHART_X1.toFixed(1),
    dotY: endY.toFixed(1),
    labelX: (CHART_X1 + 6).toFixed(1),
    labelY: endY.toFixed(1),
    labelAnchor: "start",
    labelText: `${Math.round(endFrac * 100)}% left`,
    state: "ok",
  };
}

const CHART_STATES = ["ok", "warn"];

function drawChart(chart, c) {
  c.line.setAttribute("points", chart.linePoints);
  c.dot.setAttribute("cx", chart.dotX);
  c.dot.setAttribute("cy", chart.dotY);
  c.label.setAttribute("x", chart.labelX);
  c.label.setAttribute("y", chart.labelY);
  c.label.setAttribute("text-anchor", chart.labelAnchor);
  c.label.textContent = chart.labelText;
  for (const s of CHART_STATES) {
    c.line.classList.toggle(`burn-chart__line--${s}`, s === chart.state);
    c.dot.classList.toggle(`burn-chart__dot--${s}`, s === chart.state);
  }
}

const VERDICT_STATES = ["ok", "warn", "on"];

function recompute(sloInput, measuredInput, windowGroup, ctx) {
  const slo = parseFloat(sloInput.value);
  const measured = parseFloat(measuredInput.value);
  const sloValid = Number.isFinite(slo) && slo >= 0 && slo <= 100;
  const measuredValid = Number.isFinite(measured) && measured >= 0 && measured <= 100;
  sloInput.setAttribute("aria-invalid", sloValid ? "false" : "true");
  measuredInput.setAttribute("aria-invalid", measuredValid ? "false" : "true");

  for (const chip of ctx.sloPresets) {
    const on = parseFloat(chip.dataset.slo) === slo;
    chip.classList.toggle("is-active", on);
    chip.setAttribute("aria-pressed", on);
  }

  if (!sloValid || !measuredValid) {
    for (const node of ctx.budgetNodes) node.textContent = "—";
    ctx.burnNode.textContent = "—";
    ctx.verdictNode.textContent = "";
    return;
  }

  const active = windowGroup.querySelector(".is-active");
  const windowSecs = parseFloat(active ? active.dataset.secs : "2592000");
  const r = budgetResult(slo, measured, windowSecs);

  for (const node of ctx.budgetNodes) node.textContent = r[node.dataset.budget];
  ctx.burnNode.textContent = r.burn;
  ctx.verdictNode.textContent = r.verdict;
  for (const s of VERDICT_STATES) {
    ctx.verdictNode.classList.toggle(`tool-verdict--${s}`, s === r.state);
  }

  if (ctx.chart) {
    drawChart(buildChart(slo, measured, windowSecs), ctx.chart);
    ctx.crosshair.rate = burnRate(measured, slo);
    ctx.crosshair.windowSecs = windowSecs;
    if (active) ctx.chart.window.textContent = active.dataset.window;
  }
}

// Crosshair: read budget remaining at the day under the cursor. The line is a
// deterministic function of the inputs, so this reads it rather than a dataset.
function wireCrosshair(svg, ctx) {
  const { group, line, label } = ctx.crosshairEls;
  const hit = svg.querySelector("#burn-hit");
  if (!group || !line || !label || !hit) return;

  hit.addEventListener("mousemove", (e) => {
    const box = svg.getBoundingClientRect();
    const svgX = ((e.clientX - box.left) / box.width) * 640;
    const t = Math.min(Math.max((svgX - CHART_X0) / (CHART_X1 - CHART_X0), 0), 1);
    const rate = ctx.crosshair.rate;
    const remaining = Number.isFinite(rate) ? Math.max(1 - rate * t, 0) : 0;
    const x = chartX(t);
    const days = Math.round((t * ctx.crosshair.windowSecs) / 86_400);
    line.setAttribute("x1", x.toFixed(1));
    line.setAttribute("x2", x.toFixed(1));
    label.setAttribute("x", Math.min(Math.max(x, 60), 548).toFixed(1));
    label.textContent = `day ${days} · ${Math.round(remaining * 100)}%`;
    group.classList.add("is-on");
  });
  hit.addEventListener("mouseleave", () => group.classList.remove("is-on"));
}

function init() {
  const sloInput = document.getElementById("slo-input");
  const measuredInput = document.getElementById("measured-input");
  const windowGroup = document.querySelector('[aria-label="Window"]');
  if (!sloInput || !measuredInput || !windowGroup) return;

  const ctx = {
    sloPresets: document.querySelectorAll(".tool-presets .tool-preset"),
    budgetNodes: document.querySelectorAll("[data-budget]"),
    burnNode: document.getElementById("burn-rate"),
    verdictNode: document.getElementById("verdict"),
    crosshair: { rate: 0, windowSecs: 2_592_000 },
  };

  const svg = document.querySelector(".burn-chart");
  const line = document.getElementById("burn-line");
  const dot = document.getElementById("burn-dot");
  const label = document.getElementById("burn-label");
  const windowLabel = document.getElementById("burn-window");
  if (svg && line && dot && label && windowLabel) {
    ctx.chart = { line, dot, label, window: windowLabel };
    ctx.crosshairEls = {
      group: document.getElementById("burn-cross"),
      line: document.querySelector(".burn-chart__cross-line"),
      label: document.getElementById("burn-cross-label"),
    };
    wireCrosshair(svg, ctx);
  }

  const run = () => recompute(sloInput, measuredInput, windowGroup, ctx);

  sloInput.addEventListener("input", run);
  measuredInput.addEventListener("input", run);

  for (const chip of ctx.sloPresets) {
    chip.addEventListener("click", () => {
      sloInput.value = chip.dataset.slo;
      run();
    });
  }

  for (const btn of windowGroup.querySelectorAll(".tool-preset")) {
    btn.addEventListener("click", () => {
      for (const b of windowGroup.querySelectorAll(".tool-preset")) {
        const on = b === btn;
        b.classList.toggle("is-active", on);
        b.setAttribute("aria-pressed", on);
      }
      run();
    });
  }

  run();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
