// The passkey sign-in button, hidden where the browser has no WebAuthn at all,
// plus the autofill offer on the email field beside it.
//
// The browser permits one outstanding `navigator.credentials.get()` at a time,
// and a second while one is held open is refused as "a request is already
// pending", an error indistinguishable from a dismissed prompt. So exactly one
// thing owns that fact: `offer` for the autofill request, `modal` for the
// button's. Nothing starts a ceremony without first waiting for the other to be
// released, and only `arm` and `disarm` ever assign `offer`.
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

    async function start(signal) {
        // The server decides which of these it will honour.
        const here = new URLSearchParams(window.location.search);
        const res = await fetch("/auth/passkey/login/start", {
            method: "POST",
            headers: smPasskey.headers,
            signal: signal,
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

    // ---------------------------------------------------------------------
    // Autofill
    // ---------------------------------------------------------------------
    const field = document.getElementById("magic-link-email");
    const disclosure = field && field.closest("details");
    const canArm = () => !disclosure || disclosure.open;

    let offer = null;
    let modal = false;
    let refresh = null;

    // The challenge behind a held-open request expires while it waits, so it is
    // replaced before it can refuse the pick it exists to serve. Derived from
    // what the server sent, so the two cannot drift apart.
    function refreshAfter(ttlSeconds) {
        return Math.max(30, Math.round(ttlSeconds * 0.8)) * 1000;
    }

    function clearRefresh() {
        if (refresh !== null) {
            clearTimeout(refresh);
            refresh = null;
        }
    }

    // A timer, not a focus handler: the visitor this protects is the one
    // sitting in the field reading, who fires no further events.
    function scheduleRefresh(afterMs) {
        clearRefresh();
        refresh = setTimeout(function () {
            disarm().then(arm);
        }, afterMs);
    }

    async function runOffer(controller) {
        const challenge = await start(controller.signal);
        scheduleRefresh(refreshAfter(challenge.ttl_seconds));
        const assertion = await navigator.credentials.get({
            mediation: "conditional",
            signal: controller.signal,
            publicKey: smPasskey.decodeOptions(
                challenge.options.publicKey || challenge.options,
            ),
        });
        if (!assertion) return;
        // Picking from the list is as deliberate as pressing the button, so
        // from here a failure is the visitor's to see, not the console's.
        try {
            await finish(challenge.handle, assertion);
        } catch (err) {
            show(err.message || "that did not work, try another way in");
        }
    }

    function arm() {
        if (offer || modal || !canArm()) return;
        const controller = new AbortController();
        const entry = { controller: controller, done: null };
        entry.done = runOffer(controller)
            .catch(function (err) {
                // Silent until the visitor picks something: this offer is one
                // they never asked for, and a banner over the email form they
                // were reaching for would be the wrong answer to it.
                if (!smPasskey.isAbandoned(err)) {
                    console.warn("[passkey] autofill offer unavailable:", err);
                }
            })
            .finally(function () {
                // Only when still ours; `disarm` has already moved on.
                if (offer === entry) {
                    offer = null;
                    clearRefresh();
                    arm();
                }
            });
        offer = entry;
    }

    // Resolves once the browser has let the request go, not merely once it has
    // been told to. Asking for the next one any earlier is the race.
    async function disarm() {
        const entry = offer;
        if (!entry) return;
        offer = null;
        clearRefresh();
        entry.controller.abort();
        await entry.done;
    }

    if (field && smPasskey.supported() && PublicKeyCredential.isConditionalMediationAvailable) {
        PublicKeyCredential.isConditionalMediationAvailable()
            .then(function (available) {
                if (!available) return;
                // Armed on a gesture, never at load: the ceremony row this
                // writes should answer for a visitor who reached for the email
                // field, not for every view of this page. `toggle` fires before
                // the field can take focus, which buys the round-trip the
                // autofill list needs; `focus` covers the block already being
                // open on arrival, which is how a returning visitor lands.
                if (disclosure) {
                    disclosure.addEventListener("toggle", function () {
                        if (disclosure.open) arm();
                        else disarm();
                    });
                } else {
                    // Focus alone lands in the same tick the browser builds
                    // its autofill list, too late for the round-trip.
                    arm();
                }
                field.addEventListener("focus", function () {
                    clearError();
                    arm();
                });
            })
            // No API to fall back to, so a browser that throws here simply has
            // no autofill offer. Logged because "my passkey never appears" has
            // no other evidence to start from.
            .catch(function (err) {
                console.warn("[passkey] conditional mediation unavailable:", err);
            });
    }

    btn.addEventListener("click", async function () {
        clearError();
        btn.disabled = true;
        modal = true;
        try {
            await disarm();
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
        } finally {
            modal = false;
            // The offer died with the modal ceremony, so put it back.
            arm();
        }
    });
})();
