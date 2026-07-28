// First-run org name + slug on an empty dashboard. Saves both in one submit,
// unlike /settings/team where they are separate forms: before any monitor
// exists there is nothing pointed at the slug, so the cutover warning and its
// confirm step would be noise.
(function () {
    const form = document.getElementById("org-identity");
    if (!form) return;
    const nameInput = document.getElementById("identity-name");
    const slugInput = document.getElementById("identity-slug");
    const host = document.getElementById("identity-host");
    const avail = document.getElementById("identity-avail");
    const status = document.getElementById("identity-status");
    const errors = document.getElementById("identity-errors");
    const suffix = host ? host.textContent.slice(form.dataset.currentSlug.length) : "";

    const showAvail = (text, ok) => {
        avail.textContent = text;
        avail.className = "font-mono text-xs " + (ok ? "flash-text--ok" : "flash-text--bad");
    };

    let checkTimer;
    // Called directly rather than through a synthetic input event, which would
    // be indistinguishable from real typing and immediately stop the autofill.
    const slugChanged = () => {
        status.textContent = "";
        clearTimeout(checkTimer);
        const slug = slugInput.value.trim();
        if (host) host.textContent = slug + suffix;
        // Below the minimum the answer is always "too short", which is noise
        // while someone is still typing. Submit reports it.
        if (slug.length < 3 || slug === form.dataset.currentSlug) {
            avail.textContent = "";
            return;
        }
        checkTimer = setTimeout(async () => {
            try {
                const data = await window.smOrgIdentity.checkSlug(slug);
                if (data.available) showAvail("✓ available", true);
                else showAvail(`✗ ${data.reason || "unavailable"}`, false);
            } catch { avail.textContent = ""; }
        }, 300);
    };

    slugInput.addEventListener("input", () => {
        slugInput.dataset.touched = "1";
        slugChanged();
    });

    // The generated slug is nobody's choice, so the name drives it until the
    // slug is edited by hand.
    nameInput.addEventListener("input", () => {
        if (slugInput.dataset.touched) return;
        const derived = window.smOrgIdentity.slugify(nameInput.value);
        if (!derived) return;
        slugInput.value = derived;
        slugChanged();
    });

    form.addEventListener("submit", async (ev) => {
        ev.preventDefault();
        window.smClearFormErrors(errors);
        status.textContent = "";
        const name = nameInput.value.trim();
        const slug = slugInput.value.trim();
        if (!name) {
            nameInput.setAttribute("aria-invalid", "true");
            window.smRenderClientError(errors, "Enter an organization name.");
            return;
        }
        const body = {};
        if (name !== nameInput.defaultValue) body.name = name;
        if (slug !== form.dataset.currentSlug) body.slug = slug;
        if (!Object.keys(body).length) {
            status.className = "flash-text flash-text--muted";
            status.textContent = "Nothing changed.";
            return;
        }
        const btn = form.querySelector("button[type=submit]");
        btn.disabled = true;
        try {
            const res = await window.smOrgIdentity.patch(form.dataset.orgId, body);
            if (res.ok) {
                if (body.slug) form.dataset.currentSlug = body.slug;
                nameInput.defaultValue = name;
                avail.textContent = "";
                status.className = "flash-text flash-text--ok";
                status.textContent = "Saved.";
            } else {
                window.smRenderApiError(errors, res.data, res.status);
            }
        } catch {
            window.smRenderClientError(errors, "Network error — try again.");
        } finally {
            btn.disabled = false;
        }
    });
})();
