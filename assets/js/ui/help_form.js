// Stays on the page after submit: they came here already confused, so a
// redirect that loses their words is the wrong failure mode.

(function () {
    const form = document.getElementById("help-form");
    if (!form) return;

    const banner = document.getElementById("form-errors");
    const submitBtn = form.querySelector("[data-help-submit]");
    const sentNote = form.querySelector("[data-help-sent]");
    const sentRef = form.querySelector("[data-help-ref]");
    const messageField = form.querySelector("[name=\"message\"]");

    const referrerPath = (() => {
        // An empty referrer resolves against the base and would report "/", a
        // page they never visited. Nothing beats the wrong thing.
        if (!document.referrer) return null;
        try {
            const url = new URL(document.referrer, window.location.origin);
            return url.origin === window.location.origin ? url.pathname + url.search : null;
        } catch {
            return null;
        }
    })();

    form.addEventListener("submit", async (evt) => {
        evt.preventDefault();
        if (submitBtn.disabled) return;
        window.smClearFormErrors(banner);
        sentNote.classList.add("hidden");

        const message = messageField.value.trim();
        if (!message) {
            window.smRenderClientError(banner, "Tell us what is going on first.");
            messageField.focus();
            return;
        }
        const topic = form.querySelector("[name=\"topic\"]:checked")?.value || "question";

        const label = submitBtn.textContent;
        submitBtn.disabled = true;
        submitBtn.textContent = "sending…";
        try {
            let res;
            try {
                res = await fetch("/api/v1/support", {
                    method: "POST",
                    headers: {
                        "Content-Type": "application/json",
                        "Accept": "application/json",
                        "X-Requested-With": "uptimepage",
                    },
                    body: JSON.stringify({ topic, message, page_url: referrerPath }),
                });
            } catch (err) {
                window.smRenderClientError(banner, `Network error: ${err.message || err}`);
                return;
            }
            if (res.ok) {
                // Confirm even if the ack body was unreadable; only the ref is lost.
                let ack = null;
                try { ack = await res.json(); } catch { /* keep the confirmation */ }
                sentRef.textContent = ack?.reference || "—";
                form.reset();
                sentNote.classList.remove("hidden");
                sentNote.scrollIntoView({ block: "center", behavior: "smooth" });
                return;
            }
            let body;
            try { body = await res.json(); }
            catch { window.smRenderClientError(banner, `Request failed (${res.status})`); return; }
            window.smRenderApiError(banner, body, res.status);
        } finally {
            submitBtn.disabled = false;
            submitBtn.textContent = label;
        }
    });
})();
