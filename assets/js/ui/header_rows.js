// Key/value row repeater for the HTTP headers section of the monitor form.
// Hydrates from server-rendered <div data-header-row> children, lets the
// user add/remove rows, and exposes a `collect()` helper that check_form.js
// calls on submit to build the headers JSON object.

(function () {
    const container = document.querySelector("[data-header-rows]");
    const addBtn = document.querySelector("[data-header-add]");
    const tmpl = document.getElementById("header-row-template");
    if (!container || !addBtn) return;

    function addRow(focus) {
        if (!tmpl) return null;
        const frag = tmpl.content.cloneNode(true);
        const row = frag.querySelector("[data-header-row]");
        container.appendChild(frag);
        if (focus) row.querySelector("input").focus();
        return row;
    }

    addBtn.addEventListener("click", () => addRow(true));

    // Public adder: the variable auth picker calls this to drop a prefilled
    // header row (e.g. `Authorization: Bearer {{key}}`) without duplicating the
    // row markup.
    window.smAddHeaderRow = function (name, value) {
        const rows = Array.from(container.querySelectorAll("[data-header-row]"));
        const keyEl = (r) => r.querySelector("[name='http_header_key']");
        const valEl = (r) => r.querySelector("[name='http_header_value']");
        // Reuse a row already targeting this header so the picker never adds a
        // duplicate that smCollectHeaders would silently collapse to the last.
        let row = rows.find((r) => keyEl(r).value.trim().toLowerCase() === name.toLowerCase());
        if (!row) row = rows.find((r) => keyEl(r).value.trim() === "" && valEl(r).value.trim() === "");
        if (!row) row = addRow(false);
        if (!row) return null;
        keyEl(row).value = name;
        valEl(row).value = value;
        refreshSecretHint();
        return row;
    };

    container.addEventListener("click", (evt) => {
        const btn = evt.target.closest("[data-header-remove]");
        if (!btn) return;
        const row = btn.closest("[data-header-row]");
        if (row) row.remove();
        refreshSecretHint();
    });

    // Nudge when a credential is typed straight into a header: a plaintext value
    // is stored unencrypted. A `{{var}}` reference is already the secure path,
    // so it's exempt.
    const SENSITIVE = [
        "authorization", "proxy-authorization", "cookie",
        "x-auth-token", "x-api-key", "x-amz-security-token",
    ];
    const hintEl = document.querySelector("[data-header-secret-hint]");
    const isRef = (v) => v.includes("{{");

    function refreshSecretHint() {
        if (!hintEl) return;
        const rows = [...container.querySelectorAll("[data-header-row]")].map((r) => ({
            key: r.querySelector("[name='http_header_key']").value.trim().toLowerCase(),
            val: r.querySelector("[name='http_header_value']").value.trim(),
        }));
        const hasSecret = SENSITIVE.some((k) =>
            rows.some((r) => r.key === k && r.val && !isRef(r.val)));
        if (hasSecret) {
            hintEl.textContent =
                "This looks like a credential typed in plain text. Save it as a secret variable so it stays encrypted, then reference it with {{…}} in the value.";
            hintEl.hidden = false;
        } else {
            hintEl.hidden = true;
        }
    }

    container.addEventListener("input", refreshSecretHint);
    refreshSecretHint();

    // Public collector — `check_form.js` reads this on submit + test-now.
    window.smCollectHeaders = function () {
        const out = {};
        for (const row of container.querySelectorAll("[data-header-row]")) {
            const key = row.querySelector("[name='http_header_key']").value.trim();
            const value = row.querySelector("[name='http_header_value']").value;
            if (key.length === 0) continue;
            out[key] = value;
        }
        return out;
    };
})();
