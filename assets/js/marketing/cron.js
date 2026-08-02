// Client-side cron parser: validates a standard 5-field expression, describes
// it in plain English, and computes the next run times in the viewer's local
// timezone. No backend — the server ships a static reference table + default.
import { toolError, toolUsed } from "./_tool_event.js";

const TOOL = "cron";
// Wait for typing to settle; half-typed input isn't a failed expectation.
const SETTLE_MS = 1_200;
// Shape only. Parser messages quote user tokens, so they can't be reported.
const MAX_FIELDS = 12;
const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DOWS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTH_NAMES = { JAN: 1, FEB: 2, MAR: 3, APR: 4, MAY: 5, JUN: 6, JUL: 7, AUG: 8, SEP: 9, OCT: 10, NOV: 11, DEC: 12 };
const DOW_NAMES = { SUN: 0, MON: 1, TUE: 2, WED: 3, THU: 4, FRI: 5, SAT: 6 };
const MACROS = {
  "@hourly": "0 * * * *",
  "@daily": "0 0 * * *",
  "@midnight": "0 0 * * *",
  "@weekly": "0 0 * * 0",
  "@monthly": "0 0 1 * *",
  "@yearly": "0 0 1 1 *",
  "@annually": "0 0 1 1 *",
};

function pad(n) {
  return String(n).padStart(2, "0");
}

function ordinal(n) {
  const s = ["th", "st", "nd", "rd"];
  const v = n % 100;
  return n + (s[(v - 20) % 10] || s[v] || s[0]);
}

function joinList(items) {
  if (items.length === 1) return items[0];
  if (items.length === 2) return `${items[0]} and ${items[1]}`;
  return `${items.slice(0, -1).join(", ")}, and ${items[items.length - 1]}`;
}

function normalizeNames(spec, nameMap) {
  if (!nameMap) return spec;
  return spec.replace(/[a-zA-Z]+/g, (tok) => {
    const v = nameMap[tok.toUpperCase()];
    if (v === undefined) throw new Error(`unknown name "${tok}"`);
    return String(v);
  });
}

function parseField(spec, min, max, nameMap) {
  const normalized = normalizeNames(spec, nameMap);
  const out = new Set();
  for (const part of normalized.split(",")) {
    let m;
    if (part === "*") {
      for (let i = min; i <= max; i++) out.add(i);
    } else if ((m = part.match(/^\*\/(\d+)$/))) {
      const step = +m[1];
      if (step < 1) throw new Error("step must be at least 1");
      for (let i = min; i <= max; i += step) out.add(i);
    } else if ((m = part.match(/^(\d+)-(\d+)(?:\/(\d+))?$/))) {
      const a = +m[1], b = +m[2], step = m[3] ? +m[3] : 1;
      if (step < 1) throw new Error("step must be at least 1");
      if (a < min || b > max || a > b) throw new Error(`"${part}" is out of range ${min}-${max}`);
      for (let i = a; i <= b; i += step) out.add(i);
    } else if ((m = part.match(/^(\d+)$/))) {
      const v = +m[1];
      if (v < min || v > max) throw new Error(`"${v}" is out of range ${min}-${max}`);
      out.add(v);
    } else {
      throw new Error(`can't read "${part}"`);
    }
  }
  if (out.size === 0) throw new Error("empty field");
  return out;
}

function parseCron(expr) {
  let e = expr.trim();
  if (e.startsWith("@")) {
    const mapped = MACROS[e.toLowerCase()];
    if (!mapped) throw new Error(`unknown shortcut "${e}"`);
    e = mapped;
  }
  const fields = e.split(/\s+/);
  if (fields.length !== 5) {
    throw new Error(`expected 5 fields, got ${fields.length}`);
  }
  const [mi, ho, dm, mo, dw] = fields;
  const minute = parseField(mi, 0, 59, null);
  const hour = parseField(ho, 0, 23, null);
  const dom = parseField(dm, 1, 31, null);
  const month = parseField(mo, 1, 12, MONTH_NAMES);
  const dow = parseField(dw, 0, 7, DOW_NAMES);
  if (dow.has(7)) {
    dow.add(0);
    dow.delete(7);
  }
  return { minute, hour, dom, month, dow, domRestricted: dm !== "*", dowRestricted: dw !== "*", raw: { mi, ho, dm, mo, dw } };
}

function matches(date, f) {
  if (!f.minute.has(date.getMinutes())) return false;
  if (!f.hour.has(date.getHours())) return false;
  if (!f.month.has(date.getMonth() + 1)) return false;
  const domHit = f.dom.has(date.getDate());
  const dowHit = f.dow.has(date.getDay());
  // Cron quirk: when BOTH day-of-month and day-of-week are restricted, a run
  // fires if EITHER matches; otherwise both must match.
  if (f.domRestricted && f.dowRestricted) return domHit || dowHit;
  return domHit && dowHit;
}

function nextRuns(f, count) {
  const runs = [];
  const d = new Date();
  d.setSeconds(0, 0);
  d.setMinutes(d.getMinutes() + 1);
  // ~366 days of minutes: enough to find yearly runs, bounded so an
  // impossible expression (e.g. Feb 30) terminates instead of hanging.
  const cap = 366 * 24 * 60;
  for (let i = 0; i < cap && runs.length < count; i++) {
    if (matches(d, f)) runs.push(new Date(d));
    d.setMinutes(d.getMinutes() + 1);
  }
  return runs;
}

function describeList(spec, name) {
  const label = name || ((n) => String(n));
  const pieces = spec.split(",").map((part) => {
    let m;
    if ((m = part.match(/^(\d+)-(\d+)$/))) return `${label(+m[1])} through ${label(+m[2])}`;
    if ((m = part.match(/^(\d+)$/))) return label(+m[1]);
    return part;
  });
  return joinList(pieces);
}

function minuteDesc(mi) {
  let m;
  if (mi === "*") return "every minute";
  if ((m = mi.match(/^\*\/(\d+)$/))) return `every ${ordinal(+m[1])} minute`;
  if ((m = mi.match(/^(\d+)$/))) return `minute ${+m[1]}`;
  if ((m = mi.match(/^(\d+)-(\d+)$/))) return `every minute from ${+m[1]} through ${+m[2]}`;
  return `minutes ${describeList(mi, null)}`;
}

function hourDesc(ho) {
  let m;
  if (ho === "*") return "every hour";
  if ((m = ho.match(/^\*\/(\d+)$/))) return `every ${ordinal(+m[1])} hour`;
  if ((m = ho.match(/^(\d+)$/))) return `hour ${+m[1]}`;
  if ((m = ho.match(/^(\d+)-(\d+)$/))) return `every hour from ${+m[1]} through ${+m[2]}`;
  return `hours ${describeList(ho, null)}`;
}

function timeClause(mi, ho) {
  let m;
  if (mi === "*" && ho === "*") return "Every minute";
  if (/^\d+$/.test(mi) && /^\d+$/.test(ho)) return `At ${pad(+ho)}:${pad(+mi)}`;
  if ((m = mi.match(/^\*\/(\d+)$/)) && ho === "*") return `Every ${+m[1]} minute${+m[1] > 1 ? "s" : ""}`;
  if (mi === "0" && (m = ho.match(/^\*\/(\d+)$/))) return `Every ${+m[1]} hour${+m[1] > 1 ? "s" : ""}`;
  return `At ${minuteDesc(mi)} past ${hourDesc(ho)}`;
}

function describe(raw) {
  const mi = raw.mi;
  const ho = raw.ho;
  const dm = raw.dm;
  const mo = normalizeNames(raw.mo, MONTH_NAMES);
  const dw = normalizeNames(raw.dw, DOW_NAMES);
  const parts = [timeClause(mi, ho)];
  const domText = dm === "*" ? "" : `on day ${describeList(dm, null)} of the month`;
  const dowText = dw === "*" ? "" : `on ${describeList(dw, (n) => DOWS[n % 7])}`;
  if (dm !== "*" && dw !== "*") {
    parts.push(`${domText} or ${dowText}`);
  } else if (domText) {
    parts.push(domText);
  } else if (dowText) {
    parts.push(dowText);
  }
  if (mo !== "*") parts.push(`in ${describeList(mo, (n) => MONTHS[n - 1])}`);
  return `${parts.join(", ")}.`;
}

function render(input, descNode, nextList) {
  let f;
  try {
    f = parseCron(input.value);
  } catch (err) {
    input.setAttribute("aria-invalid", "true");
    descNode.textContent = err.message;
    descNode.classList.add("tool-cron__desc--error");
    nextList.replaceChildren();
    return false;
  }
  input.setAttribute("aria-invalid", "false");
  descNode.textContent = describe(f.raw);
  descNode.classList.remove("tool-cron__desc--error");

  const runs = nextRuns(f, 5);
  const fmt = { weekday: "short", year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" };
  const items = runs.length
    ? runs.map((d) => {
        const li = document.createElement("li");
        li.className = "tool-cron__run mk-mono";
        li.textContent = d.toLocaleString(undefined, fmt);
        return li;
      })
    : [Object.assign(document.createElement("li"), { className: "tool-cron__run mk-quiet", textContent: "no runs in the next year" })];
  nextList.replaceChildren(...items);
  return true;
}

function init() {
  const input = document.getElementById("cron-input");
  if (!input) return;
  const descNode = document.getElementById("cron-desc");
  const nextList = document.getElementById("cron-next");
  const presets = document.querySelectorAll(".tool-cron-presets .tool-preset");

  let settle;
  const run = () => {
    const valid = render(input, descNode, nextList);
    const current = input.value.trim();
    for (const b of presets) {
      const on = b.dataset.cron === current;
      b.classList.toggle("is-active", on);
      b.setAttribute("aria-pressed", on);
    }
    return valid;
  };

  input.addEventListener("input", () => {
    const valid = run();
    toolUsed(TOOL, { mode: "expression" });
    clearTimeout(settle);
    if (valid) return;
    settle = setTimeout(() => {
      const expr = input.value.trim();
      if (!expr) return;
      toolError(TOOL, {
        fields: Math.min(expr.split(/\s+/).length, MAX_FIELDS),
        macro: expr.startsWith("@"),
      });
    }, SETTLE_MS);
  });

  for (const b of presets) {
    b.addEventListener("click", () => {
      input.value = b.dataset.cron;
      run();
      clearTimeout(settle);
      toolUsed(TOOL, { mode: "preset", preset: b.dataset.cron });
    });
  }
  run();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
