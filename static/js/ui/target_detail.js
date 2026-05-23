// Detail-page "Run check now": POSTs /api/v1/targets/{id}/check-now and
// renders the persisted CheckResult inline. Reuses smRunCheckNow +
// smRenderCheckResult from api_form.js.

(function () {
    const btn = document.querySelector("[data-detail-test-now]");
    const resultEl = document.querySelector("[data-detail-test-result]");
    if (!btn || !resultEl) return;

    btn.addEventListener("click", async () => {
        const id = btn.dataset.targetId;
        if (!id) return;
        btn.disabled = true;
        window.smRenderCheckRunning(resultEl);
        try {
            const r = await window.smRunCheckNow(id);
            if (!r.ok) {
                const code = (r.body && r.body.error && r.body.error.code)
                    || (r.networkError ? "network" : `HTTP ${r.status}`);
                const message = (r.body && r.body.error && r.body.error.message)
                    || (r.networkError ? String(r.networkError.message || r.networkError)
                                       : "Check rejected.");
                window.smRenderCheckError(resultEl, `${code}: ${message}`);
                return;
            }
            window.smRenderCheckResult(resultEl, r.body || {}, {
                footnote: "Reload the page to see this result in the charts.",
            });
        } finally {
            btn.disabled = false;
        }
    });
})();
