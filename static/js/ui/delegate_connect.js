// Public /c/<code> connect page: manual create, the telegram QR, and the
// consumed-poll that flips any successful path (form, OAuth on another
// device, telegram webhook) to the success state.
(function () {
    "use strict";

    const root = document.getElementById("connect-root");
    if (!root) return;
    const code = root.dataset.code;

    const success = root.querySelector("[data-connect-success]");
    const options = root.querySelector("[data-connect-options]");
    function showSuccess() {
        options?.classList.add("hidden");
        success?.classList.remove("hidden");
    }

    // ── telegram deep link + QR ─────────────────────────────────────────
    const tgQr = root.querySelector("[data-tg-qr]");
    const tgLink = root.querySelector("[data-tg-link]");
    const tgGroup = root.querySelector("[data-tg-group]");
    function renderQr(el, url) {
        if (typeof qrcode !== "function") {
            el.classList.add("hidden");
            return;
        }
        const qr = qrcode(0, "M");
        qr.addData(url);
        qr.make();
        el.innerHTML = qr.createSvgTag({ cellSize: 4, margin: 0, scalable: true });
        const svg = el.querySelector("svg");
        if (svg) {
            svg.style.width = "164px";
            svg.style.height = "164px";
            svg.style.display = "block";
        }
        el.classList.remove("hidden");
    }
    function renderTgDest() {
        if (!tgLink) return;
        const url = tgGroup?.checked ? tgLink.dataset.groupLink : tgLink.dataset.privateLink;
        tgLink.textContent = url;
        tgLink.href = url;
        if (tgQr) renderQr(tgQr, url);
    }
    tgGroup?.addEventListener("change", renderTgDest);
    if (tgLink) {
        // qrcode.min.js is deferred too; wait for full load before drawing.
        if (document.readyState === "complete") renderTgDest();
        else window.addEventListener("load", renderTgDest);
    }

    // ── consumed poll ───────────────────────────────────────────────────
    const poll = setInterval(async () => {
        let body;
        try {
            const res = await fetch(`/c/${code}/status`, { headers: { "Accept": "application/json" } });
            if (!res.ok) return;
            body = await res.json();
        } catch {
            return;
        }
        if (body.status === "consumed") {
            clearInterval(poll);
            showSuccess();
        } else if (body.status === "expired") {
            clearInterval(poll);
            window.location.reload();
        }
    }, 5000);

    // ── manual form ─────────────────────────────────────────────────────
    const form = root.querySelector("[data-connect-form]");
    if (!form) return;
    const kindSelect = form.querySelector("[data-kind-select]");
    const valueInput = form.querySelector('[name="value"]');
    const valueLabel = form.querySelector("[data-value-label]");
    const valueHint = form.querySelector("[data-value-hint]");
    const errorBox = form.querySelector("[data-form-error]");

    const FIELDS = {
        slack: { label: "Slack incoming webhook URL", type: "url", placeholder: "https://hooks.slack.com/services/T000/B000/XXXX", hint: "" },
        discord: { label: "Discord webhook URL", type: "url", placeholder: "https://discord.com/api/webhooks/0000/XXXX", hint: "" },
        msteams: { label: "Teams Workflows webhook URL", type: "url", placeholder: "https://prod-00.westus.logic.azure.com/workflows/…", hint: "" },
        google_chat: { label: "Google Chat webhook URL", type: "url", placeholder: "https://chat.googleapis.com/v1/spaces/XXXX/messages?key=…&token=…", hint: "" },
        webhook: { label: "Webhook URL", type: "url", placeholder: "https://example.com/hooks/alerts", hint: "Receives one JSON POST per alert." },
        email: { label: "Recipient address", type: "email", placeholder: "oncall@example.com", hint: "A verification mail is sent — alerts start after its link is confirmed." },
    };

    function configFor(kind, value) {
        switch (kind) {
            case "slack":
            case "discord":
            case "msteams":
            case "google_chat":
                return { type: kind, webhook_url: value };
            case "webhook":
                return { type: "webhook", url: value, headers: {} };
            case "email":
                return { type: "email", to: value };
            default:
                return null;
        }
    }

    function renderKind() {
        const f = FIELDS[kindSelect.value] || FIELDS.webhook;
        valueLabel.textContent = f.label;
        valueInput.type = f.type;
        valueInput.placeholder = f.placeholder;
        valueHint.textContent = f.hint;
        valueHint.classList.toggle("hidden", !f.hint);
    }
    kindSelect.addEventListener("change", renderKind);
    renderKind();

    function showError(msg) {
        errorBox.textContent = msg;
        errorBox.classList.remove("hidden");
    }

    form.addEventListener("submit", async (evt) => {
        evt.preventDefault();
        errorBox.classList.add("hidden");
        const kind = kindSelect.value;
        const value = valueInput.value.trim();
        if (!value) {
            showError(`${FIELDS[kind]?.label || "Value"} is required.`);
            valueInput.focus();
            return;
        }
        const config = configFor(kind, kind === "email" ? value.toLowerCase() : value);
        const name = form.querySelector('[name="name"]').value.trim();
        const submit = form.querySelector('button[type="submit"]');
        submit.disabled = true;
        try {
            const res = await fetch(`/c/${code}/create`, {
                method: "POST",
                headers: { "Content-Type": "application/json", "Accept": "application/json" },
                body: JSON.stringify(name ? { name, config } : { config }),
            });
            if (!res.ok) {
                let msg = `connect failed (${res.status})`;
                try {
                    const body = await res.json();
                    msg = body?.error?.message || msg;
                } catch { /* keep fallback */ }
                showError(msg);
                return;
            }
            showSuccess();
        } catch (err) {
            showError(`network error: ${err.message || err}`);
        } finally {
            submit.disabled = false;
        }
    });
})();
