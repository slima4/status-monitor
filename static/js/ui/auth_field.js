(function () {
    // Redaction state machine for credential fields.
    //
    // Each <fieldset data-auth-field="basic|bearer" data-initial-mode="…" data-mode="…">
    // tracks one of three states:
    //
    //   create    — no credentials yet. Inputs disabled+empty; toggle off.
    //   redacted  — credentials exist server-side as REDACTED_SENTINEL. Inputs disabled+sentinel; toggle off.
    //   replacing — user is supplying real values. Inputs enabled+empty; toggle on.
    //
    // Toggle off returns to whichever state was the initial template-rendered one.
    // On submit, check_form.js inspects data-mode: it includes basic_auth/bearer_token
    // in the POST/PATCH body only when data-mode === "replacing".

    const REDACTED_SENTINEL = "***";

    document.addEventListener("change", (evt) => {
        const t = evt.target;
        if (!t.matches("[data-auth-toggle]")) return;
        const fs = t.closest("[data-auth-field]");
        if (!fs) return;
        const initial = fs.dataset.initialMode || "create";
        const inputs = fs.querySelectorAll("input[name^='http_']");

        if (t.checked) {
            fs.dataset.mode = "replacing";
            inputs.forEach(i => {
                i.disabled = false;
                i.value = "";
            });
            if (inputs[0]) inputs[0].focus();
        } else {
            fs.dataset.mode = initial;
            inputs.forEach(i => {
                i.disabled = true;
                i.value = initial === "redacted" ? REDACTED_SENTINEL : "";
            });
        }
    });
})();
