// Incident console actions: acknowledge / resolve / reopen / add-note / declare.
// Posts to /api/v1/incidents/* (same-origin session cookie + X-Requested-With,
// the custom header that gates state-changing requests). Reloads on success so
// the row/detail reflects the new state.
(function () {
  "use strict";
  // The dashboard banner that hosts this script lives inside a partial htmx
  // re-swaps every 5s, which re-executes the tag. Bind the delegated listeners
  // exactly once or each refresh stacks another → N duplicate POSTs per click.
  if (window.__smIncidentsConsoleInit) return;
  window.__smIncidentsConsoleInit = true;
  const root = document;

  function showError(msg) {
    const b = root.querySelector("[data-incident-error]");
    if (b) {
      b.textContent = msg;
      b.classList.remove("hidden");
    }
    if (window.smToast) window.smToast({ message: msg, kind: "error" });
  }

  async function post(url, body) {
    const r = await fetch(url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Accept: "application/json",
        "X-Requested-With": "uptimepage",
      },
      body: JSON.stringify(body || {}),
    });
    let json = null;
    try {
      json = await r.json();
    } catch (_e) {
      /* empty body */
    }
    return { ok: r.ok, status: r.status, json: json };
  }

  function errMsg(res) {
    const e = (res.json && res.json.error) || {};
    const code = e.code ? e.code + ": " : "";
    return code + (e.message || "request failed (" + res.status + ")");
  }

  async function runAction(id, action) {
    if (!id || !action) return;
    const verb = { acknowledge: "Acknowledge", resolve: "Resolve", reopen: "Reopen" }[action];
    if (!verb) return;
    if (window.smConfirm) {
      const ok = await window.smConfirm({ title: verb + " incident?", body: "", confirmLabel: verb });
      if (!ok) return;
    }
    const res = await post("/api/v1/incidents/" + encodeURIComponent(id) + "/" + action, {});
    if (!res.ok) return showError(errMsg(res));
    if (window.smToast) window.smToast({ message: verb + "d", kind: "ok" });
    window.location.reload();
  }

  async function addNote(id) {
    if (!id || !window.smPrompt) return;
    const msg = await window.smPrompt({
      title: "Add note",
      body: "Recorded on the incident's internal timeline.",
      placeholder: "what's happening…",
      multiline: true,
    });
    if (!msg) return;
    const res = await post("/api/v1/incidents/" + encodeURIComponent(id) + "/notes", { message: msg });
    if (!res.ok) return showError(errMsg(res));
    window.location.reload();
  }

  async function submitDeclare(form) {
    const fd = new FormData(form);
    const tid = (fd.get("target_id") || "").toString().trim();
    const body = {
      title: (fd.get("title") || "").toString().trim(),
      severity: (fd.get("severity") || "major").toString(),
      urgency: (fd.get("urgency") || "high").toString(),
    };
    if (tid) body.target_id = tid;
    const res = await post("/api/v1/incidents", body);
    if (!res.ok) return showError(errMsg(res));
    const id = res.json && res.json.id;
    window.location.href = id ? "/incidents/" + id : "/incidents";
  }

  root.addEventListener("click", function (ev) {
    const act = ev.target.closest("[data-incident-action]");
    if (act) {
      ev.preventDefault();
      return runAction(act.dataset.incidentId, act.dataset.incidentAction);
    }
    const note = ev.target.closest("[data-incident-note]");
    if (note) {
      ev.preventDefault();
      return addNote(note.dataset.incidentId);
    }
  });

  root.addEventListener("submit", function (ev) {
    const form = ev.target.closest("[data-incident-declare-form]");
    if (form) {
      ev.preventDefault();
      submitDeclare(form);
    }
  });
})();
