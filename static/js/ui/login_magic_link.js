// Magic-link request form on /login. POSTs the email, swaps the form for the
// "check your inbox" panel — the response shape is identical for known and
// unknown addresses (anti-enumeration), so success is the only end state a
// visitor can observe.
(function () {
    const form = document.getElementById("magic-link-form");
    if (!form) return;
    const banner = document.getElementById("magic-link-error");
    const sent = document.getElementById("magic-link-sent");
    const email = document.getElementById("magic-link-email");

    form.addEventListener("submit", async (ev) => {
        ev.preventDefault();
        if (!email.value || !email.checkValidity()) {
            email.setAttribute("aria-invalid", "true");
            window.smRenderClientError(banner, "Enter a valid email address.");
            return;
        }
        window.smClearFormErrors(banner);
        const btn = form.querySelector("button[type=submit]");
        btn.disabled = true;
        try {
            const res = await fetch("/auth/magic-link/request", {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "X-Requested-With": "uptimepage",
                },
                body: JSON.stringify({ email: email.value.trim() }),
            });
            if (!res.ok) {
                const msg = await window.smApiErrorMessage(res, `request failed (${res.status})`);
                window.smRenderClientError(banner, msg);
                btn.disabled = false;
                return;
            }
            form.classList.add("hidden");
            sent.classList.remove("hidden");
        } catch {
            window.smRenderClientError(banner, "Network error — try again.");
            btn.disabled = false;
        }
    });
})();
