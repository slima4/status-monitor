// Sign-in methods on the account page: add a provider, remove one.
(function () {
    "use strict";

    const box = document.querySelector("[data-sign-in-methods]");
    if (!box) return;
    const headers = { "Accept": "application/json", "X-Requested-With": "uptimepage" };

    function failed(what, err) {
        window.smToast({ message: what + ": " + err.message, kind: "error" });
    }

    // POST so the CSRF guard covers it, which means the header rides a fetch,
    // which a 302 would not survive. So the URL comes back as JSON.
    box.querySelectorAll("[data-link-url]").forEach(function (btn) {
        btn.addEventListener("click", async function () {
            try {
                const r = await fetch(btn.dataset.linkUrl, { method: "POST", headers });
                if (!r.ok) {
                    throw new Error(await window.smApiErrorMessage(r, "HTTP " + r.status));
                }
                const body = await r.json();
                window.location = body.url;
            } catch (err) {
                failed("could not start", err);
            }
        });
    });

    box.querySelectorAll("[data-identity-remove]").forEach(function (btn) {
        btn.addEventListener("click", async function () {
            const row = btn.closest("[data-identity]");
            const url = "/api/v1/me/sign-in-methods/" + encodeURIComponent(row.dataset.provider)
                + "?provider_user_id=" + encodeURIComponent(row.dataset.subject);
            try {
                const r = await fetch(url, { method: "DELETE", headers });
                if (!r.ok) {
                    throw new Error(await window.smApiErrorMessage(r, "HTTP " + r.status));
                }
                // Which row is now the last one is decided server-side.
                window.location.reload();
            } catch (err) {
                failed("could not remove", err);
            }
        });
    });
})();
