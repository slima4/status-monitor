// Incident postmortem editor: serialises the summary/root-cause/impact +
// action-item rows into a PUT /api/v1/incidents/{id}/postmortem body, and
// drives the publish/unpublish buttons. Shares api_form.js for error rendering.
(function () {
  "use strict";
  const root = document.querySelector("[data-postmortem]");
  if (!root) return;
  const incidentId = root.getAttribute("data-incident-id");
  const form = document.getElementById("postmortem-form");
  const items = document.getElementById("action-items");
  const tmpl = document.getElementById("action-item-template");
  const banner = document.getElementById("form-errors");

  document.getElementById("add-action-item").addEventListener("click", () => {
    items.appendChild(tmpl.content.cloneNode(true));
    if (window.smInitComboboxes) window.smInitComboboxes();
  });

  items.addEventListener("click", (ev) => {
    const rm = ev.target.closest("[data-ai-remove]");
    if (rm) rm.closest("[data-action-item]").remove();
  });

  function buildBody() {
    const val = (name) => (form.querySelector(`[name=${name}]`).value || "").trim();
    const action_items = Array.from(items.querySelectorAll("[data-action-item]"))
      .map((row) => ({
        text: (row.querySelector("[data-ai-text]").value || "").trim(),
        owner_user_id: row.querySelector("[data-ai-owner]").value || null,
        done: row.querySelector("[data-ai-done]").checked,
      }))
      .filter((a) => a.text.length > 0);
    return {
      summary: val("summary") || null,
      root_cause: val("root_cause") || null,
      impact: val("impact") || null,
      action_items: action_items,
    };
  }

  async function send(url, method, body) {
    const r = await fetch(url, {
      method: method,
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

  const submitBtn = form.querySelector("button[type=submit]");
  form.addEventListener("submit", async (ev) => {
    ev.preventDefault();
    if (submitBtn.disabled) return;
    window.smClearFormErrors(banner);
    const label = submitBtn.textContent;
    submitBtn.disabled = true;
    submitBtn.textContent = "saving…";
    let navigating = false;
    try {
      const res = await send("/api/v1/incidents/" + encodeURIComponent(incidentId) + "/postmortem", "PUT", buildBody());
      if (res.ok) {
        navigating = true;
        window.location = "/incidents/" + encodeURIComponent(incidentId);
        return;
      }
      window.smRenderApiError(banner, res.json, res.status);
    } finally {
      if (!navigating) {
        submitBtn.disabled = false;
        submitBtn.textContent = label;
      }
    }
  });

  root.addEventListener("click", async (ev) => {
    const btn = ev.target.closest("[data-postmortem-publish]");
    if (!btn) return;
    const publish = btn.getAttribute("data-postmortem-publish") === "true";
    const action = publish ? "publish" : "unpublish";
    const base = "/api/v1/incidents/" + encodeURIComponent(incidentId) + "/postmortem";
    window.smClearFormErrors(banner);
    // Persist current edits first so publish promotes what the operator sees,
    // not the last saved revision.
    const saved = await send(base, "PUT", buildBody());
    if (!saved.ok) {
      window.smRenderApiError(banner, saved.json, saved.status);
      return;
    }
    const res = await send(base + "/" + action, "POST", {});
    if (!res.ok) {
      window.smRenderApiError(banner, res.json, res.status);
      return;
    }
    window.location.reload();
  });
})();
