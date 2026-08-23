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

    // Not the platform-authenticator probe: this ceremony names no credential,
    // so a USB key or a phone over hybrid answers it too.
    if (smPasskey.supported()) btn.hidden = false;

    btn.addEventListener("click", async function () {
        if (error) error.hidden = true;
        btn.disabled = true;
        try {
            // The server decides which of these it will honour.
            const here = new URLSearchParams(window.location.search);
            const started = await fetch("/auth/passkey/login/start", {
                method: "POST",
                headers: smPasskey.headers,
                body: JSON.stringify({
                    redirect_after: here.get("redirect_after"),
                    invitation: here.get("invitation"),
                }),
            });
            if (!started.ok) throw new Error("HTTP " + started.status);
            const challenge = await started.json();

            const assertion = await navigator.credentials.get({
                publicKey: smPasskey.decodeOptions(challenge.options.publicKey || challenge.options),
            });
            if (!assertion) throw new Error("no passkey was offered");

            const finished = await fetch("/auth/passkey/login/finish", {
                method: "POST",
                headers: smPasskey.headers,
                body: JSON.stringify({
                    handle: challenge.handle,
                    credential: smPasskey.encodeCredential(assertion),
                }),
            });
            if (!finished.ok) {
                // smApiErrorMessage rides api_form.js, which only loads with the
                // magic-link block, so it may not be here.
                const fallback = "HTTP " + finished.status;
                throw new Error(
                    window.smApiErrorMessage
                        ? await window.smApiErrorMessage(finished, fallback)
                        : fallback,
                );
            }
            const body = await finished.json();
            window.location = body.redirect;
        } catch (err) {
            // A dismissed prompt is not a problem; the button just comes back.
            if (!smPasskey.isAbandoned(err)) {
                show(err.message || "that did not work, try another way in");
            }
            btn.disabled = false;
        }
    });
})();
