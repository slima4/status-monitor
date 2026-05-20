// Status-page slug rename.
//
// PATCHes the org's slug via `/api/v1/orgs/{id}`. The server applies the same
// validation as create (`validate_slug`) and rejects taken slugs with
// SLUG_TAKEN — both surface as field-scoped errors through the shared
// `smRenderApiError` banner. On success we reload so the displayed URL (and
// every other slug-derived link on the page) matches the new value.
(function () {
  const form = document.getElementById("slug-form");
  if (!form) return;
  const action = form.dataset.action;
  const banner = document.getElementById("slug-form-errors");
  const result = document.getElementById("slug-result");
  const input = document.getElementById("slug-input");
  const initial = (input.value || "").trim();
  const HEADERS = {
    "Accept": "application/json",
    "Content-Type": "application/json",
    "X-Requested-With": "status-monitor",
  };

  function clear() {
    window.smClearFormErrors(banner);
    result.textContent = "";
    result.className = "text-sm";
  }

  form.addEventListener("submit", async (evt) => {
    evt.preventDefault();
    clear();
    const slug = (input.value || "").trim().toLowerCase();
    if (slug === initial) {
      result.textContent = "No change.";
      result.className = "text-sm text-slate-500";
      return;
    }
    try {
      const res = await fetch(action, {
        method: "PATCH",
        headers: HEADERS,
        body: JSON.stringify({ slug }),
      });
      if (!res.ok) {
        let body;
        try { body = await res.json(); }
        catch {
          window.smRenderClientError(banner, `Request failed (${res.status})`);
          return;
        }
        window.smRenderApiError(banner, body, res.status, {
          onField: (field) => {
            if (field === "slug") {
              input.setAttribute("aria-invalid", "true");
              input.focus({ preventScroll: true });
            }
          },
        });
        return;
      }
      // The new URL lives in the page header link; cheapest correct reflection
      // is a reload (it also picks up the suffix preview for any other slug-
      // derived UI without us re-templating it client-side).
      result.textContent = "Saved — reloading…";
      result.className = "text-sm text-emerald-700";
      window.location.reload();
    } catch (err) {
      window.smRenderClientError(banner, `Network error: ${err.message || err}`);
    }
  });
})();
