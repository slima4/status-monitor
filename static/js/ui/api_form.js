// Shared helpers for the JSON-API-backed forms (monitor form, notification
// channel form). The page-specific scripts keep their own buildBody/field
// logic; everything below is the common error-banner + escaping layer, loaded
// (deferred) before those scripts. Single source so a fix on the
// security-sensitive escaping path can't miss one form.

window.smEscapeHtml = function (s) {
    return String(s).replace(/[&<>"']/g, ch => ({
        "&": "&amp;",
        "<": "&lt;",
        ">": "&gt;",
        "\"": "&quot;",
        "'": "&#39;",
    }[ch]));
};

window.smClearFormErrors = function (banner) {
    banner.innerHTML = "";
    banner.classList.add("hidden");
    document.querySelectorAll("[aria-invalid]").forEach(el => el.removeAttribute("aria-invalid"));
};

window.smRenderClientError = function (banner, msg) {
    banner.textContent = msg;
    banner.classList.remove("hidden");
};

// `opts.messageFor(err)` → optional replacement message (e.g. SSRF copy);
// `opts.onField(field)` → optional per-field affordance (focus/aria-invalid).
window.smRenderApiError = function (banner, json, status, opts) {
    opts = opts || {};
    const err = (json && json.error) || {};
    const code = err.code || `HTTP ${status}`;
    let message = err.message || "Request rejected.";
    if (opts.messageFor) {
        const override = opts.messageFor(err);
        if (override) message = override;
    }
    banner.innerHTML = `<strong>${window.smEscapeHtml(code)}</strong>: ${window.smEscapeHtml(message)}`;
    if (err.field) {
        banner.insertAdjacentHTML("beforeend",
            ` <span class="text-xs text-red-600">(field: ${window.smEscapeHtml(err.field)})</span>`);
    }
    banner.classList.remove("hidden");
    banner.scrollIntoView({ block: "center", behavior: "smooth" });
    if (err.field && opts.onField) opts.onField(err.field);
};
