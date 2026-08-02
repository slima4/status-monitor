// Live uptime -> allowed-downtime calculator. The page ships a fully
// server-rendered default state, so this only upgrades it to recompute on
// input. Math mirrors the Rust side so hydration never changes the numbers.
import { toolUsed } from "./_tool_event.js";

const TOOL = "uptime-sla";
const DAY = 86_400;
const WEEK = 604_800;
const MONTH = 2_592_000; // 30 days
const YEAR = 31_536_000; // 365 days
const CHECK_INTERVAL = 60;

const PERIODS = { day: DAY, week: WEEK, month: MONTH, year: YEAR };

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

function recompute(input, valueNodes, checksNode, presets) {
  const uptime = parseFloat(input.value);
  const valid = Number.isFinite(uptime) && uptime >= 0 && uptime <= 100;
  input.setAttribute("aria-invalid", valid ? "false" : "true");

  for (const node of valueNodes) {
    const period = PERIODS[node.dataset.downtime];
    node.textContent = valid ? humanDuration(period * (1 - uptime / 100)) : "—";
  }

  if (checksNode) {
    const missed = valid ? Math.round((MONTH * (1 - uptime / 100)) / CHECK_INTERVAL) : 0;
    checksNode.textContent = valid ? missed.toLocaleString() : "—";
  }

  for (const chip of presets) {
    const on = parseFloat(chip.dataset.uptime) === uptime;
    chip.classList.toggle("is-active", on);
    chip.setAttribute("aria-pressed", on);
  }
}

const UNIT_SECS = { second: 1, minute: 60, hour: 3_600 };

function formatPct(pct) {
  return `${parseFloat(pct.toFixed(4))}%`;
}

function activeData(group, key, fallback) {
  const active = group.querySelector(".is-active");
  return active ? active.dataset[key] : fallback;
}

function recomputeReverse(input, unitGroup, periodGroup, out) {
  const value = parseFloat(input.value);
  const unit = UNIT_SECS[activeData(unitGroup, "unit", "hour")];
  const period = PERIODS[activeData(periodGroup, "period", "month")];
  const downSecs = Number.isFinite(value) ? Math.max(value, 0) * unit : NaN;
  const uptime = (1 - downSecs / period) * 100;
  out.textContent = Number.isFinite(uptime) ? formatPct(Math.max(uptime, 0)) : "—";
}

function wireGroup(group, run, onUse) {
  for (const btn of group.querySelectorAll(".tool-preset")) {
    btn.addEventListener("click", () => {
      for (const b of group.querySelectorAll(".tool-preset")) {
        const on = b === btn;
        b.classList.toggle("is-active", on);
        b.setAttribute("aria-pressed", on);
      }
      run();
      onUse();
    });
  }
}

function init() {
  const input = document.getElementById("uptime-input");
  if (input) {
    const valueNodes = document.querySelectorAll("[data-downtime]");
    const checksNode = document.getElementById("tool-checks");
    const presets = document.querySelectorAll(".tool-presets .tool-preset");
    const run = () => recompute(input, valueNodes, checksNode, presets);
    input.addEventListener("input", () => {
      run();
      toolUsed(TOOL, { mode: "uptime-to-downtime" });
    });
    for (const chip of presets) {
      chip.addEventListener("click", () => {
        input.value = chip.dataset.uptime;
        run();
        toolUsed(TOOL, { mode: "uptime-to-downtime", preset: chip.dataset.uptime });
      });
    }
    // Hydration, not use.
    run();
  }

  const down = document.getElementById("down-input");
  const unitGroup = document.querySelector('[aria-label="Downtime unit"]');
  const periodGroup = document.querySelector('[aria-label="Period"]');
  const out = document.getElementById("down-uptime");
  if (down && unitGroup && periodGroup && out) {
    const runReverse = () => recomputeReverse(down, unitGroup, periodGroup, out);
    const usedReverse = () => toolUsed(TOOL, { mode: "downtime-to-uptime" });
    down.addEventListener("input", () => {
      runReverse();
      usedReverse();
    });
    wireGroup(unitGroup, runReverse, usedReverse);
    wireGroup(periodGroup, runReverse, usedReverse);
    runReverse();
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
