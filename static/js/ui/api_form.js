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

// The server's error message from a failed fetch Response, or `fallback`
// when the body isn't the standard error envelope.
window.smApiErrorMessage = async function (res, fallback) {
    try {
        const json = await res.json();
        if (json?.error?.message) return json.error.message;
    } catch { /* not JSON */ }
    return fallback;
};

// POST /api/v1/targets/{id}/check-now (best-effort). Returns the parsed
// body on success or null on any failure — callers handle null themselves.
window.smRunCheckNow = async function (id) {
    try {
        const r = await fetch(`/api/v1/targets/${id}/check-now`, {
            method: "POST",
            headers: {
                "Accept": "application/json",
                "X-Requested-With": "uptimepage",
            },
        });
        let body = null;
        try { body = await r.json(); } catch { /* empty body */ }
        return { ok: r.ok, status: r.status, body };
    } catch (err) {
        return { ok: false, status: 0, body: null, networkError: err };
    }
};

// Render a CheckResult-shaped object into an element using the
// .test-result* CSS classes. `extra.matched` (bool) adjusts the pill
// label; `extra.headers` (Vec) + `extra.body` (str) append HTTP-only
// detail sections. Returns nothing.
window.smRenderCheckResult = function (el, result, extra) {
    if (!el) return;
    extra = extra || {};
    const status = (result && result.status) || "unknown";
    const matched = extra.matched !== undefined ? !!extra.matched : status === "up";
    const klass = matched && status === "up"
        ? "test-result--ok"
        : status === "degraded"
            ? "test-result--warn"
            : "test-result--bad";
    el.className = `test-result ${klass}`;
    const pillLabel = matched ? status.toUpperCase()
        : `${status.toUpperCase()} — expectation failed`;
    const meta = [
        result.duration_ms != null ? `${result.duration_ms} ms` : null,
        result.response_code != null ? `HTTP ${result.response_code}` : null,
        result.response_size != null ? `${result.response_size} B` : null,
    ].filter(Boolean).join(" · ");
    let html = `
        <div class="flex flex-wrap items-center gap-2">
            <span class="test-result__pill">${window.smEscapeHtml(pillLabel)}</span>
            <span class="test-result__meta">${window.smEscapeHtml(meta)}</span>
        </div>
    `;
    if (result.error) {
        html += `<div class="test-result__meta mt-1">${window.smEscapeHtml(result.error)}</div>`;
    }
    const headers = extra.headers || [];
    if (headers.length > 0) {
        const rows = headers
            .map(h => `${window.smEscapeHtml(h.name)}: ${window.smEscapeHtml(h.value)}`)
            .join("\n");
        html += `<details class="mt-2"><summary class="cursor-pointer text-xs">Response headers (${headers.length})</summary><pre class="test-result__body">${rows}</pre></details>`;
    }
    if (extra.body) {
        html += `<details class="mt-2" open><summary class="cursor-pointer text-xs">Response body (first 1 KiB)</summary><pre class="test-result__body">${window.smEscapeHtml(extra.body)}</pre></details>`;
    }
    if (extra.footnote) {
        html += `<div class="test-result__meta mt-1 italic">${window.smEscapeHtml(extra.footnote)}</div>`;
    }
    el.innerHTML = html;
    el.classList.remove("hidden");
};

window.smRenderCheckRunning = function (el) {
    if (!el) return;
    // Live-region backstop: the templates set role="status" so the first
    // update is announced; self-heal any container that forgot it.
    if (!el.hasAttribute("role")) el.setAttribute("role", "status");
    el.className = "test-result";
    el.textContent = "Running…";
    el.classList.remove("hidden");
};

window.smRenderCheckError = function (el, msg) {
    if (!el) return;
    el.className = "test-result test-result--bad";
    el.textContent = msg;
    el.classList.remove("hidden");
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
            ` <span class="text-xs text-state-bad">(field: ${window.smEscapeHtml(err.field)})</span>`);
    }
    banner.classList.remove("hidden");
    banner.scrollIntoView({ block: "center", behavior: "smooth" });
    if (err.field && opts.onField) opts.onField(err.field);
};
