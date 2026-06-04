// "Channels that page you" toggles on /settings/on-call. Each change replaces
// the member's full contact set via PUT /api/v1/on-call/my-contacts.
(function () {
    const root = document.querySelector("[data-contacts]");
    if (!root) return;
    const result = root.querySelector("[data-contacts-result]");

    function show(msg, ok) {
        if (!result) return;
        result.textContent = msg;
        result.className = "flash-text text-xs " + (ok ? "flash-text--ok" : "flash-text--bad");
    }

    function currentIds() {
        return Array.from(root.querySelectorAll("[data-contact]:checked")).map((c) => c.value);
    }

    let inFlight = false;
    root.addEventListener("change", async (evt) => {
        if (!evt.target.closest("[data-contact]")) return;
        if (inFlight) return;
        inFlight = true;
        const boxes = root.querySelectorAll("[data-contact]");
        boxes.forEach((b) => (b.disabled = true));
        try {
            const res = await fetch("/api/v1/on-call/my-contacts", {
                method: "PUT",
                headers: {
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "X-Requested-With": "uptimepage",
                },
                body: JSON.stringify({ channel_ids: currentIds() }),
            });
            if (res.ok) {
                show("✓ saved", true);
            } else {
                let msg = "save failed";
                try { const b = await res.json(); if (b && b.error && b.error.message) msg = b.error.message; } catch { /* */ }
                show("✗ " + msg, false);
            }
        } catch {
            show("✗ network error", false);
        } finally {
            boxes.forEach((b) => (b.disabled = false));
            inFlight = false;
        }
    });
})();
