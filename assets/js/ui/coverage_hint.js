// Dismissals are per host and browser-local: a hint is not worth a round trip,
// and the panel goes away for good once the checks it offers exist.
(function () {
    const KEY = "sm.coverage.dismissed";

    function dismissed() {
        try {
            const raw = JSON.parse(localStorage.getItem(KEY));
            return Array.isArray(raw) ? raw : [];
        } catch {
            return [];
        }
    }

    function remember(host) {
        try {
            const all = dismissed();
            if (!all.includes(host)) {
                localStorage.setItem(KEY, JSON.stringify(all.concat(host).slice(-200)));
            }
        } catch {
            // Private mode or a full quota: the panel just comes back.
        }
    }

    for (const panel of document.querySelectorAll("[data-coverage]")) {
        if (!dismissed().includes(panel.dataset.coverage)) panel.hidden = false;
    }

    document.addEventListener("click", (evt) => {
        const btn = evt.target.closest("[data-coverage-dismiss]");
        if (!btn) return;
        const panel = btn.closest("[data-coverage]");
        if (!panel) return;
        remember(panel.dataset.coverage);
        panel.hidden = true;
    });
})();
