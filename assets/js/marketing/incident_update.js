// Privacy-first incident copy generator. All transformation stays in-browser;
// there are no fetches, storage calls, analytics payloads, or URL parameters.
function clean(value, fallback) {
  const text = value.trim().replace(/\s+/g, " ");
  return text || fallback;
}

function sentence(value) {
  const text = clean(value, "");
  if (!text) return "";
  return /[.!?]$/.test(text) ? text : `${text}.`;
}

function lowerFirst(value) {
  return value ? value.charAt(0).toLowerCase() + value.slice(1) : value;
}

const PRESETS = {
  website: ["Website", "The website is currently unavailable", "Our engineers are working to restore access", "No action is required"],
  api: ["API", "Some API requests are failing", "Our engineers are investigating the errors", "Please retry failed requests"],
  performance: ["Application", "Pages and requests are loading more slowly than usual", "Our engineers are investigating elevated latency", "No action is required"],
  login: ["Authentication", "Some customers cannot sign in", "Our engineers are investigating authentication failures", "Please wait before retrying"],
  payments: ["Payments", "Some payment attempts are failing", "Our engineers are investigating with our payment provider", "Please do not submit the same payment repeatedly"],
  regional: ["Regional services", "Customers in one region may be unable to access the service", "Our engineers are rerouting traffic and investigating the regional failure", "Customers outside the affected region do not need to take action"],
};

function incidentCopy(state) {
  const service = clean(state.service, "service");
  const impact = sentence(clean(state.impact, "Customers may experience disruption"));
  const action = sentence(clean(state.action, "Our team is working to restore normal service"));
  const next = clean(state.next, "within 30 minutes");
  const again = `We will share another update ${lowerFirst(next)}.`;
  const guidance = state.guidance.trim() ? ` ${sentence(state.guidance)}` : "";

  const variants = {
    investigating: [`Investigating ${service} issues`, `${impact}${guidance} ${action} ${again}`],
    identified: [`Cause identified for ${service}`, `${impact}${guidance} We have identified the cause. ${action} ${again}`],
    monitoring: [`${service} service recovering`, `${impact}${guidance} A mitigation is in place. ${action} We are monitoring recovery and will update you again ${lowerFirst(next)}.`],
    resolved: [`${service} incident resolved`, `${impact} Service has returned to normal and this incident is resolved.${guidance} We are reviewing the event and will share follow-up information if appropriate.`],
  };
  return variants[state.phase];
}

function maintenanceCopy(state) {
  const service = clean(state.service, "service");
  const impact = sentence(clean(state.impact, "A brief interruption may occur"));
  const action = sentence(clean(state.action, "Our team will monitor the work throughout the maintenance window"));
  const next = clean(state.next, "when the work is complete");
  const guidance = state.guidance.trim() ? ` ${sentence(state.guidance)}` : "";
  const variants = {
    scheduled: [`Scheduled maintenance for ${service}`, `We will perform scheduled maintenance on ${service}. ${impact}${guidance} ${action} We will post another update ${lowerFirst(next)}.`],
    "in-progress": [`${service} maintenance in progress`, `Scheduled maintenance on ${service} is now in progress. ${impact}${guidance} ${action} We will post another update ${lowerFirst(next)}.`],
    completed: [`${service} maintenance complete`, `Scheduled maintenance on ${service} is complete. The service is operating normally.${guidance}`],
  };
  return variants[state.maintenancePhase];
}

function isTerminal(state) {
  return (state.type === "incident" && state.phase === "resolved") ||
    (state.type === "maintenance" && state.maintenancePhase === "completed");
}

function emailWrap(state, message) {
  const service = clean(state.service, "our service");
  const intro = state.type === "maintenance"
    ? `Hello,\n\nWe are writing about scheduled maintenance affecting ${service}. `
    : `Hello,\n\nWe are writing to let you know about an issue affecting ${service}. `;
  const outro = isTerminal(state)
    ? "\n\nThank you for your patience. You can follow future updates on our status page."
    : "\n\nWe understand the disruption this may cause and appreciate your patience. We will continue to share updates on our status page.";
  return `${intro}${message}${outro}`;
}

function init() {
  const fields = {
    service: document.getElementById("incident-service"),
    impact: document.getElementById("incident-impact"),
    action: document.getElementById("incident-action"),
    guidance: document.getElementById("incident-guidance"),
    next: document.getElementById("incident-next"),
    timezone: document.getElementById("incident-timezone"),
  };
  const title = document.getElementById("incident-output-title");
  const body = document.getElementById("incident-output-body");
  const time = document.getElementById("incident-output-time");
  const copy = document.getElementById("incident-copy");
  const copyStatus = document.getElementById("incident-copy-status");
  const incidentPhases = document.getElementById("incident-phases");
  const maintenancePhases = document.getElementById("maintenance-phases");
  const presetsField = document.getElementById("incident-presets-field");
  if (!title || !body || !time || !copy || Object.values(fields).some((node) => !node)) return;

  const state = { type: "incident", phase: "investigating", maintenancePhase: "scheduled", format: "status" };
  let copiedTimer;

  function render() {
    for (const [key, node] of Object.entries(fields)) state[key] = node.value;
    const output = state.type === "incident" ? incidentCopy(state) : maintenanceCopy(state);
    title.textContent = output[0];
    body.textContent = state.format === "email" ? emailWrap(state, output[1]) : output[1];
    time.hidden = isTerminal(state);
    const nextFallback = state.type === "maintenance" ? "when the work is complete" : "within 30 minutes";
    time.textContent = `Next update · ${clean(state.next, nextFallback)} · ${state.timezone}`;
    copy.classList.remove("is-copied");
    renderQuality();
  }

  function renderQuality() {
    const terminal = isTerminal(state);
    const checks = {
      impact: state.impact.trim().length >= 12,
      action: terminal || state.action.trim().length >= 12,
      next: terminal || state.next.trim().length >= 5,
      guidance: state.guidance.trim().length >= 5,
    };
    let score = 0;
    for (const [key, pass] of Object.entries(checks)) {
      const item = document.querySelector(`[data-check="${key}"]`);
      if (!item) continue;
      item.classList.toggle("is-pass", pass);
      item.classList.toggle("is-advice", !pass);
      item.querySelector("span").textContent = pass ? "✓" : "+";
      if (pass) score += 1;
    }
    document.getElementById("quality-score").textContent = `${score}/4`;
    document.getElementById("quality-bar").style.setProperty("--quality", `${score * 25}%`);
  }

  for (const group of document.querySelectorAll("[data-segment]")) {
    group.addEventListener("click", (event) => {
      const button = event.target.closest("button[data-value]");
      if (!button) return;
      state[group.dataset.segment] = button.dataset.value;
      for (const peer of group.querySelectorAll("button")) {
        const on = peer === button;
        peer.classList.toggle("is-active", on);
        peer.setAttribute("aria-pressed", String(on));
      }
      if (group.dataset.segment === "type") {
        const maintenance = state.type === "maintenance";
        incidentPhases.hidden = maintenance;
        maintenancePhases.hidden = !maintenance;
        presetsField.hidden = maintenance;
      }
      render();
    });
  }

  for (const node of Object.values(fields)) node.addEventListener("input", render);

  document.querySelector(".incident-presets").addEventListener("click", (event) => {
    const button = event.target.closest("button[data-preset]");
    if (!button) return;
    const preset = PRESETS[button.dataset.preset];
    [fields.service.value, fields.impact.value, fields.action.value, fields.guidance.value] = preset;
    for (const peer of document.querySelectorAll(".incident-presets button")) peer.classList.toggle("is-active", peer === button);
    render();
    fields.service.focus();
  });

  copy.addEventListener("click", async () => {
    const message = `${title.textContent}\n\n${body.textContent}${time.hidden ? "" : `\n\n${time.textContent}`}`;
    try {
      await navigator.clipboard.writeText(message);
    } catch (_) {
      const area = document.createElement("textarea");
      area.value = message;
      area.setAttribute("readonly", "");
      area.style.position = "fixed";
      area.style.opacity = "0";
      document.body.appendChild(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    clearTimeout(copiedTimer);
    copy.classList.add("is-copied");
    if (copyStatus) copyStatus.textContent = "Message copied to clipboard";
    copiedTimer = setTimeout(() => {
      copy.classList.remove("is-copied");
      if (copyStatus) copyStatus.textContent = "";
    }, 1800);
  });

  render();
}

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
else init();
