// Magic-link form on /login. Response shape is identical for known and
// unknown addresses, so the sent panel is the only observable end state.
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
            // Email login lands where an OAuth login would.
            const page = new URLSearchParams(window.location.search);
            const res = await fetch("/auth/magic-link/request", {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "X-Requested-With": "uptimepage",
                },
                body: JSON.stringify({
                    email: email.value.trim(),
                    redirect_after: page.get("redirect_after") || null,
                    invitation: page.get("invitation") || null,
                }),
            });
            if (!res.ok) {
                const msg = await window.smApiErrorMessage(res, `request failed (${res.status})`);
                window.smRenderClientError(banner, msg);
                btn.disabled = false;
                window.umami?.track("login-error", {
                    reason: res.status === 429 ? "rate-limited" : "request-failed",
                    method: "magic-link",
                });
                return;
            }
            form.classList.add("hidden");
            sent.classList.remove("hidden");
            // OAuth fires on click; for email, an accepted request is the commit.
            window.umami?.track("login-method", {
                method: "magic-link",
                "last-used": form.dataset.lastUsed === "true",
            });
        } catch {
            window.smRenderClientError(banner, "Network error — try again.");
            btn.disabled = false;
            window.umami?.track("login-error", {
                reason: "network",
                method: "magic-link",
            });
        }
    });
})();
