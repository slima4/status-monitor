// The passkey sign-in button, hidden where the browser has no WebAuthn at all.
(function () {
    "use strict";

    const btn = document.getElementById("sm-passkey-signin");
    if (!btn || !window.smPasskey) return;
    const error = document.getElementById("sm-passkey-error");

    function show(message) {
        if (!error) return;
        error.textContent = message;
        error.hidden = false;
    }

    function clearError() {
        if (error) error.hidden = true;
    }

    async function start() {
        // The server decides which of these it will honour.
        const here = new URLSearchParams(window.location.search);
        const res = await fetch("/auth/passkey/login/start", {
            method: "POST",
            headers: smPasskey.headers,
            body: JSON.stringify({
                redirect_after: here.get("redirect_after"),
                invitation: here.get("invitation"),
            }),
        });
        if (!res.ok) throw new Error(await smPasskey.errorDetail(res));
        return await res.json();
    }

    async function finish(handle, assertion) {
        const res = await fetch("/auth/passkey/login/finish", {
            method: "POST",
            headers: smPasskey.headers,
            body: JSON.stringify({
                handle: handle,
                credential: smPasskey.encodeCredential(assertion),
            }),
        });
        if (!res.ok) throw new Error(await smPasskey.errorDetail(res));
        window.location = (await res.json()).redirect;
    }

    if (smPasskey.supported()) btn.hidden = false;

    btn.addEventListener("click", async function () {
        clearError();
        btn.disabled = true;
        try {
            const challenge = await start();
            const assertion = await navigator.credentials.get({
                publicKey: smPasskey.decodeOptions(
                    challenge.options.publicKey || challenge.options,
                ),
            });
            if (!assertion) throw new Error("no passkey was offered");
            await finish(challenge.handle, assertion);
        } catch (err) {
            // A dismissed prompt is not a problem; the button just comes back.
            if (!smPasskey.isAbandoned(err)) {
                show(err.message || "that did not work, try another way in");
            }
            btn.disabled = false;
        }
    });
})();
