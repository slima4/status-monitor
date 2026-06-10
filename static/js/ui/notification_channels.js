(function () {
    const form = document.getElementById("channel-form");
    if (!form) return;

    const isEdit = form.dataset.mode === "edit";
    const replaceCb = form.querySelector("[data-replace-config]");
    const configFs = form.querySelector("[data-config]");
    const kindFs = form.querySelector("[data-kind-chooser]");
    const nameInput = form.querySelector("[data-name-input]");
    const resultEl = form.querySelector("[data-test-result]");

    function currentKind() {
        return form.querySelector("input[name='kind']:checked")?.value || "slack";
    }

    const storedKind = currentKind();

    // True when the form's transport config is live: create, or edit with
    // "replace config" on. Gates BOTH what save submits and what the test
    // button exercises — false routes the test to the stored config.
    function usesFormConfig() {
        return !isEdit || !!(replaceCb && replaceCb.checked);
    }

    function syncTestHint() {
        const hint = form.querySelector("[data-test-hint]");
        if (!hint) return;
        hint.textContent = usesFormConfig() ? hint.dataset.hintForm : hint.dataset.hintStored;
    }

    function hideTestResult() {
        resultEl?.classList.add("hidden");
    }

    function showVariant(kind) {
        form.querySelectorAll("[data-variant]").forEach(el => {
            el.classList.toggle("hidden", el.dataset.variant !== kind);
        });
    }

    function syncNamePlaceholder() {
        if (nameInput) nameInput.placeholder = `ops-${currentKind()}`;
    }

    // On edit the secret is never shown; the config inputs stay disabled
    // until the operator opts to replace the whole transport config. The
    // type cards lock with them (one fieldset.disabled flip) — the kind is
    // part of the config, so a diverged card with no config submit would
    // lie about what gets saved. The revert below is cosmetic: the server
    // derives the stored kind from the config on PATCH regardless.
    function syncConfigEnabled() {
        if (!isEdit) return;
        const on = !!(replaceCb && replaceCb.checked);
        configFs.querySelectorAll("input, textarea, select").forEach(el => {
            if (el === replaceCb) return;
            el.disabled = !on;
        });
        if (kindFs) kindFs.disabled = !on;
        if (!on) {
            const stored = form.querySelector(`input[name='kind'][value='${storedKind}']`);
            if (stored) stored.checked = true;
            showVariant(storedKind);
            syncNamePlaceholder();
        }
        form.querySelector("[data-kind-locked-hint]")?.classList.toggle("hidden", on);
        syncTestHint();
    }

    function syncAll() {
        showVariant(currentKind());
        syncNamePlaceholder();
        syncConfigEnabled();
        syncTestHint();
    }

    syncAll();
    // Browsers restore form-control state on reload / history navigation
    // without firing change events — re-derive everything from it.
    window.addEventListener("pageshow", syncAll);

    form.addEventListener("change", (evt) => {
        if (evt.target.name === "kind") {
            showVariant(currentKind());
            syncNamePlaceholder();
            hideTestResult();
        }
    });
    // A green "✓ delivered" must not vouch for a config edited after the
    // test ran.
    form.addEventListener("input", (evt) => {
        if (evt.target.closest("[data-config]")) hideTestResult();
    });
    if (replaceCb) replaceCb.addEventListener("change", syncConfigEnabled);

    // "Test now": exercises the same notifier path a real incident uses.
    // Create / replace-config: POSTs the form's config without saving;
    // locked edit: tests the stored config by id.
    const testBtn = form.querySelector("[data-send-test]");
    if (testBtn) {
        const showResult = (text, cls) => {
            resultEl.textContent = text;
            resultEl.className = `font-mono text-xs ${cls}`;
            resultEl.classList.remove("hidden");
        };
        testBtn.addEventListener("click", async () => {
            clearErrors();
            let url = `${form.dataset.action}/test`;
            let body = null;
            if (usesFormConfig()) {
                const built = buildConfig();
                if (built.error) {
                    showResult(`✗ ${built.error}`, "flash-text flash-text--bad");
                    if (built.field) form.querySelector(`[name="${built.field}"]`)?.focus();
                    return;
                }
                // The prefilled "***" masks are not real secrets; the API
                // would reject them with PATCH-flavoured advice that the
                // test button can't follow.
                if (JSON.stringify(built.config).includes("***")) {
                    showResult(
                        isEdit
                            ? "✗ masked secrets (***) can't be tested — re-enter the real value, or untick \"Replace transport config\" to test the stored config"
                            : "✗ masked secrets (***) can't be tested — enter the real value",
                        "flash-text flash-text--bad",
                    );
                    return;
                }
                url = "/api/v1/notification-channels/test";
                body = JSON.stringify({ config: built.config });
            }
            testBtn.disabled = true;
            showResult("# sending test alert…", "text-quiet");
            try {
                const res = await fetch(url, {
                    method: "POST",
                    headers: {
                        "Content-Type": "application/json",
                        "Accept": "application/json",
                        "X-Requested-With": "uptimepage",
                    },
                    body,
                });
                if (res.ok) {
                    const enabledOn = !!form.querySelector("input[name='enabled']")?.checked;
                    showResult(
                        enabledOn
                            ? "✓ test alert delivered — check the destination"
                            : "✓ test alert delivered — note: with Enabled off, bound monitors won't alert through this channel",
                        "flash-text flash-text--ok font-medium",
                    );
                } else {
                    let msg = `delivery failed (${res.status})`;
                    try {
                        const json = await res.json();
                        if (json?.error?.message) msg = json.error.message;
                    } catch { /* keep the status fallback */ }
                    showResult(`✗ ${msg}`, "flash-text flash-text--bad font-medium");
                }
            } catch (err) {
                showResult(`✗ network error: ${err.message || err}`, "flash-text flash-text--bad font-medium");
            } finally {
                testBtn.disabled = false;
            }
        });
    }

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
        if (usesFormConfig()) {
            const built = buildConfig();
            if (built.error) return built;
            payload.config = built.config;
        }
        return { payload };
    }

    // The transport config exactly as the API expects it, from the current
    // form values. Shared by save and "test now".
    function buildConfig() {
        const data = new FormData(form);
        const kind = currentKind();
        if (kind === "slack") {
            return {
                config: {
                    type: "slack",
                    webhook_url: (data.get("slack_webhook_url") || "").trim(),
                },
            };
        }
        if (kind === "webhook") {
            let headers;
            try {
                const raw = (data.get("webhook_headers") || "").trim();
                headers = raw ? JSON.parse(raw) : {};
                if (typeof headers !== "object" || headers === null || Array.isArray(headers)) {
                    throw new Error("not an object");
                }
            } catch {
                return { error: "Headers must be a JSON object — e.g. {\"Authorization\": \"Bearer …\"}", field: "webhook_headers" };
            }
            const config = {
                type: "webhook",
                url: (data.get("webhook_url") || "").trim(),
                headers,
            };
            const secret = (data.get("webhook_secret") || "").trim();
            if (secret) {
                config.secret = secret;
            }
            return { config };
        }
        return {
            config: {
                type: "telegram",
                bot_token: (data.get("telegram_bot_token") || "").trim(),
                chat_id: (data.get("telegram_chat_id") || "").trim(),
            },
        };
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
