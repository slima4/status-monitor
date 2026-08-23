// Adding and removing a passkey from the account page.
(function () {
    "use strict";

    const box = document.querySelector("[data-sign-in-methods]");
    if (!box || !window.smPasskey) return;

    const addRow = document.getElementById("sm-passkey-add-row");
    const addBtn = document.getElementById("sm-passkey-add");

    function platformHint() {
        const d = navigator.userAgentData;
        if (d && d.platform) return d.platform;
        const ua = navigator.userAgent || "";
        const known = ["iPhone", "iPad", "Android", "Macintosh", "Windows", "Linux"];
        const hit = known.find(function (name) { return ua.indexOf(name) !== -1; });
        return hit === "Macintosh" ? "macOS" : hit || null;
    }

    function failed(what, err) {
        window.smToast({ message: what + ": " + err.message, kind: "error" });
    }

    // Same rule the sign-in button follows: a security key or a phone can mint
    // one on a machine with no built-in authenticator.
    if (addRow && addBtn) {
        if (smPasskey.supported()) addRow.hidden = false;

        addBtn.addEventListener("click", async function () {
            addBtn.disabled = true;
            try {
                const started = await fetch("/auth/passkey/register/start", {
                    method: "POST",
                    headers: smPasskey.headers,
                    body: "{}",
                });
                if (!started.ok) throw new Error(await smPasskey.errorDetail(started));
                const challenge = await started.json();

                const created = await navigator.credentials.create({
                    publicKey: smPasskey.decodeOptions(
                        challenge.options.publicKey || challenge.options,
                    ),
                });
                if (!created) throw new Error("no passkey was created");

                const finished = await fetch("/auth/passkey/register/finish", {
                    method: "POST",
                    headers: smPasskey.headers,
                    body: JSON.stringify({
                        handle: challenge.handle,
                        // Not asked for: one passkey needs no name, and the
                        // row already carries its dates.
                        nickname: platformHint(),
                        credential: smPasskey.encodeCredential(created),
                    }),
                });
                if (!finished.ok) throw new Error(await smPasskey.errorDetail(finished));
                window.location.reload();
            } catch (err) {
                if (!smPasskey.isAbandoned(err)) failed("could not add a passkey", err);
                addBtn.disabled = false;
            }
        });
    }

    box.querySelectorAll("[data-passkey-remove]").forEach(function (btn) {
        btn.addEventListener("click", async function () {
            const row = btn.closest("[data-passkey]");
            if (!row) return;
            btn.disabled = true;
            try {
                const res = await fetch(
                    "/api/v1/me/passkeys/" + encodeURIComponent(row.dataset.passkeyId),
                    { method: "DELETE", headers: smPasskey.headers },
                );
                if (!res.ok) throw new Error(await smPasskey.errorDetail(res));
                window.location.reload();
            } catch (err) {
                failed("could not remove it", err);
                btn.disabled = false;
            }
        });
    });
})();
