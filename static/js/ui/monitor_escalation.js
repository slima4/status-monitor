// Per-monitor escalation-policy binding on the monitor detail page. Saves to
// PUT /api/v1/targets/{id}/escalation-policy on change. value "" clears the
// binding (the monitor falls back to the org default). Owner-only on the API;
// a non-owner sees the 403 message in the result span.
(function () {
    const sel = document.querySelector("[data-monitor-policy-select]");
    if (!sel) return;
    const result = document.querySelector("[data-monitor-policy-result]");
    const show = (msg, ok) => {
        if (!result) return;
        result.textContent = msg;
        result.className = "flash-text text-xs " + (ok ? "flash-text--ok" : "flash-text--bad");
    };
    sel.addEventListener("change", async () => {
        const targetId = sel.dataset.targetId;
        const policyId = sel.value || null;
        sel.disabled = true;
        try {
            const res = await fetch(`/api/v1/targets/${targetId}/escalation-policy`, {
                method: "PUT",
                headers: {
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "X-Requested-With": "uptimepage",
                },
                body: JSON.stringify({ policy_id: policyId }),
            });
            if (res.ok) {
                show("✓ saved", true);
            } else {
                let msg = "save failed";
                try {
                    const b = await res.json();
                    if (b && b.error && b.error.message) msg = b.error.message;
                } catch { /* non-JSON body */ }
                show("✗ " + msg, false);
            }
        } catch (err) {
            show("✗ network error", false);
        } finally {
            sel.disabled = false;
        }
    });
})();
