// Status-page settings submit.
//
// One "Save changes" applies branding *and* the logo: a staged file is
// uploaded (or the current logo removed) first, then the branding PATCH runs.
// Errors come back through the shared `smRenderApiError` banner, and a
// field-scoped rejection (e.g. BRANDING_INVALID) highlights the offending
// input — see issues #25/#26.
(function () {
  const form = document.getElementById("status-page-form");
  if (!form) return;
  const action = form.dataset.action;
  const banner = document.getElementById("form-errors");
  const result = document.getElementById("form-result");
  const fileInput = document.getElementById("logo-file");
  const fileName = document.getElementById("logo-filename");
  const removeBox = document.getElementById("logo-remove");
  const logoImg = document.getElementById("logo-current");
  const logoNone = document.getElementById("logo-none");
  const logoWrap = document.getElementById("logo-remove-wrap");

  // Enforced cap mirrors the server's `max_logo_size_bytes`; a pre-flight
  // check gives instant feedback and skips a doomed upload round-trip.
  const MAX_LOGO_BYTES = Number(form.dataset.maxLogoBytes) || 0;
  const MAX_LOGO_LABEL = form.dataset.maxLogoLabel || "the limit";
  const HEADERS = { "Accept": "application/json", "X-Requested-With": "status-monitor" };
  // Thrown after a banner has been rendered, so the submit handler's catch
  // can tell a handled API rejection from a raw network failure.
  const HANDLED = Symbol("handled");

  function clear() {
    window.smClearFormErrors(banner);
    result.textContent = "";
    result.className = "text-sm";
  }
  // Renders the API error (with field highlight) and signals "already shown".
  async function fail(res) {
    let body;
    try { body = await res.json(); }
    catch {
      window.smRenderClientError(banner, `Request failed (${res.status})`);
      throw HANDLED;
    }
    window.smRenderApiError(banner, body, res.status, {
      onField: (field) => {
        const el = form.querySelector(`[name="${field}"]`);
        if (el) {
          el.setAttribute("aria-invalid", "true");
          el.focus({ preventScroll: true });
        }
      },
    });
    throw HANDLED;
  }

  function resetFilePicker() {
    fileInput.value = "";
    fileName.textContent = "";
  }

  // A staged file and "remove" are mutually exclusive; picking one clears
  // the other so the save intent is never ambiguous. An over-cap file is
  // rejected here so the operator isn't left waiting on a doomed upload.
  fileInput.addEventListener("change", () => {
    const f = fileInput.files[0];
    if (f && MAX_LOGO_BYTES && f.size > MAX_LOGO_BYTES) {
      resetFilePicker();
      window.smRenderClientError(
        banner,
        `That image is larger than the ${MAX_LOGO_LABEL} logo limit.`,
      );
      return;
    }
    fileName.textContent = f ? f.name : "";
    if (f && removeBox) removeBox.checked = false;
  });
  if (removeBox) {
    removeBox.addEventListener("change", () => {
      if (removeBox.checked) resetFilePicker();
    });
  }

  function settingsBody() {
    const str = (name) => {
      const v = (form.elements[name].value || "").trim();
      return v.length ? v : null;
    };
    return {
      public_status_enabled: form.elements.public_status_enabled.checked,
      public_display_name: str("public_display_name"),
      public_about: str("public_about"),
      public_brand_color: str("public_brand_color"),
      public_show_powered_by: form.elements.public_show_powered_by.checked,
    };
  }

  // null = nothing to do, "removed", or the new logo URL / "uploaded".
  // Throws HANDLED (banner shown) on an HTTP failure.
  async function applyLogo() {
    if (removeBox && removeBox.checked) {
      const res = await fetch(`${action}/logo`, { method: "DELETE", headers: HEADERS });
      if (!res.ok) await fail(res); // 204 is ok, so !res.ok already excludes it
      return "removed";
    }
    const file = fileInput.files[0];
    if (!file) return null;
    const fd = new FormData();
    fd.append("file", file);
    const res = await fetch(`${action}/logo`, { method: "POST", headers: HEADERS, body: fd });
    if (!res.ok) await fail(res);
    const json = await res.json().catch(() => null);
    return json && json.logo_url ? json.logo_url : "uploaded";
  }

  function setHasLogo(has) {
    logoImg.classList.toggle("hidden", !has);
    logoNone.classList.toggle("hidden", has);
    logoWrap.classList.toggle("hidden", !has);
  }

  function reflectLogo(outcome) {
    if (outcome === "removed") {
      logoImg.removeAttribute("src");
      if (removeBox) removeBox.checked = false;
      setHasLogo(false);
    } else {
      // A versioned URL busts the cache; "uploaded" with no URL just refreshes.
      logoImg.src = outcome === "uploaded"
        ? `${logoImg.src.split("?")[0]}?v=${Date.now()}`
        : outcome;
      setHasLogo(true);
    }
    resetFilePicker();
  }

  form.addEventListener("submit", async (evt) => {
    evt.preventDefault();
    clear();
    try {
      const logoOutcome = await applyLogo();
      const res = await fetch(action, {
        method: "PATCH",
        headers: { ...HEADERS, "Content-Type": "application/json" },
        body: JSON.stringify(settingsBody()),
      });
      if (!res.ok) await fail(res);
      if (logoOutcome) reflectLogo(logoOutcome);
      result.textContent = "Saved.";
      result.className = "flash-text flash-text--ok";
    } catch (err) {
      if (err === HANDLED) return; // banner already rendered
      window.smRenderClientError(banner, `Network error: ${err.message || err}`);
    }
  });
})();
