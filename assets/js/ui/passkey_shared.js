// WebAuthn talks in ArrayBuffers; JSON does not. Both ceremonies need the same
// translation, so it lives here rather than twice.
(function () {
    "use strict";

    function fromBase64Url(value) {
        const padded = value.replace(/-/g, "+").replace(/_/g, "/");
        const raw = atob(padded + "===".slice((padded.length + 3) % 4));
        const bytes = new Uint8Array(raw.length);
        for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
        return bytes.buffer;
    }

    function toBase64Url(buffer) {
        const bytes = new Uint8Array(buffer);
        let raw = "";
        for (let i = 0; i < bytes.length; i++) raw += String.fromCharCode(bytes[i]);
        return btoa(raw).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    }

    // The server sends base64url, so the fields the spec types as BufferSource
    // are decoded on the way in.
    function decodeOptions(options) {
        const out = Object.assign({}, options);
        out.challenge = fromBase64Url(options.challenge);
        if (options.user) {
            out.user = Object.assign({}, options.user, { id: fromBase64Url(options.user.id) });
        }
        ["excludeCredentials", "allowCredentials"].forEach(function (key) {
            if (Array.isArray(options[key])) {
                out[key] = options[key].map(function (c) {
                    return Object.assign({}, c, { id: fromBase64Url(c.id) });
                });
            }
        });
        return out;
    }

    function encodeCredential(credential) {
        const r = credential.response;
        const out = {
            id: credential.id,
            rawId: toBase64Url(credential.rawId),
            type: credential.type,
            extensions: credential.getClientExtensionResults(),
            response: { clientDataJSON: toBase64Url(r.clientDataJSON) },
        };
        if (r.attestationObject) {
            out.response.attestationObject = toBase64Url(r.attestationObject);
        }
        if (r.authenticatorData) {
            out.response.authenticatorData = toBase64Url(r.authenticatorData);
            out.response.signature = toBase64Url(r.signature);
            out.response.userHandle = r.userHandle ? toBase64Url(r.userHandle) : null;
        }
        return out;
    }

    // A cancelled prompt and a real failure both arrive as exceptions; only the
    // second is worth showing.
    function isAbandoned(err) {
        return err && (err.name === "NotAllowedError" || err.name === "AbortError");
    }

    // smApiErrorMessage rides api_form.js, which does not load on every page
    // that talks to these endpoints, so the status line is the fallback.
    async function errorDetail(res) {
        const fallback = "HTTP " + res.status;
        return window.smApiErrorMessage
            ? await window.smApiErrorMessage(res, fallback)
            : fallback;
    }

    window.smPasskey = {
        supported: function () {
            return Boolean(window.PublicKeyCredential && navigator.credentials);
        },
        headers: { "Accept": "application/json", "Content-Type": "application/json", "X-Requested-With": "uptimepage" },
        errorDetail: errorDetail,
        decodeOptions: decodeOptions,
        encodeCredential: encodeCredential,
        isAbandoned: isAbandoned,
    };
})();
