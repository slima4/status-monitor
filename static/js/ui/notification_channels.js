(function () {
    const form = document.getElementById("channel-form");
    if (!form) return;

    const isEdit = form.dataset.mode === "edit";
    const replaceCb = form.querySelector("[data-replace-config]");
    const configFs = form.querySelector("[data-config]");

    function currentKind() {
        return form.querySelector("input[name='kind']:checked")?.value || "slack";
    }

    // On edit the secret is never shown; the config inputs stay disabled
    // until the operator opts to replace the whole transport config.
    function syncConfigEnabled() {
        if (!isEdit) return;
        const on = !!(replaceCb && replaceCb.checked);
        configFs.querySelectorAll("input, textarea, select").forEach(el => {
            if (el === replaceCb) return;
            el.disabled = !on;
        });
    }

    function showVariant(kind) {
        form.querySelectorAll("[data-variant]").forEach(el => {
            el.classList.toggle("hidden", el.dataset.variant !== kind);
        });
    }

    showVariant(currentKind());
    syncConfigEnabled();

    form.addEventListener("change", (evt) => {
        if (evt.target.name === "kind") showVariant(currentKind());
    });
    if (replaceCb) replaceCb.addEventListener("change", syncConfigEnabled);

    const submitBtn = form.querySelector("button[type=submit]");
    form.addEventListener("submit", async (evt) => {
        evt.preventDefault();
        if (submitBtn.disabled) return;
        clearErrors();
        const built = buildBody();
        if (built.error) {
            renderClientError(built.error);
            if (built.field) form.querySelector(`[name="${built.field}"]`)?.focus();
            return;
        }

        const label = submitBtn.textContent;
        submitBtn.disabled = true;
        submitBtn.textContent = "saving…";
        let navigating = false;
        try {
            let res;
            try {
                res = await fetch(form.dataset.action, {
                    method: form.dataset.method,
                    headers: {
                        "Content-Type": "application/json",
                        "Accept": "application/json",
                        "X-Requested-With": "uptimepage",
                    },
                    body: JSON.stringify(built.payload),
                });
            } catch (err) {
                renderClientError(`Network error: ${err.message || err}`);
                return;
            }
            if (res.ok) { navigating = true; window.location = "/settings/notifications"; return; }
            let body;
            try { body = await res.json(); }
            catch { renderClientError(`Request failed (${res.status})`); return; }
            renderApiError(body, res.status);
        } finally {
            if (!navigating) { submitBtn.disabled = false; submitBtn.textContent = label; }
        }
    });

    function buildBody() {
        const data = new FormData(form);
        const name = (data.get("name") || "").trim();
        if (!name) return { error: "Name is required.", field: "name" };
        const payload = {
            name,
            enabled: data.get("enabled") === "on",
        };

        // Edit + "replace config" unchecked: omit config so the stored
        // secret is preserved (the API rejects a re-submitted "***").
        const sendConfig = !isEdit || (replaceCb && replaceCb.checked);
        if (sendConfig) {
            const kind = currentKind();
            if (kind === "slack") {
                payload.config = {
                    type: "slack",
                    webhook_url: (data.get("slack_webhook_url") || "").trim(),
                };
            } else if (kind === "webhook") {
                let headers;
                try {
                    const raw = (data.get("webhook_headers") || "").trim();
                    headers = raw ? JSON.parse(raw) : {};
                    if (typeof headers !== "object" || headers === null || Array.isArray(headers)) {
                        throw new Error("not an object");
                    }
                } catch {
                    return { error: "Headers must be a JSON object — e.g. {\"Authorization\": \"Bearer …\"}" };
                }
                payload.config = {
                    type: "webhook",
                    url: (data.get("webhook_url") || "").trim(),
                    headers,
                };
                const secret = (data.get("webhook_secret") || "").trim();
                if (secret) {
                    payload.config.secret = secret;
                }
            } else {
                payload.config = {
                    type: "telegram",
                    bot_token: (data.get("telegram_bot_token") || "").trim(),
                    chat_id: (data.get("telegram_chat_id") || "").trim(),
                };
            }
        }
        return { payload };
    }

    function clearErrors() {
        window.smClearFormErrors(document.getElementById("form-errors"));
    }

    function renderClientError(msg) {
        window.smRenderClientError(document.getElementById("form-errors"), msg);
    }

    function renderApiError(json, status) {
        window.smRenderApiError(document.getElementById("form-errors"), json, status);
    }
})();
