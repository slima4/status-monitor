// Org rename + slug availability behind /settings/team. Kept apart from the
// page so the client's notion of a valid slug has one definition to drift from.
(function () {
    const headers = {
        "Content-Type": "application/json",
        "Accept": "application/json",
        "X-Requested-With": "uptimepage",
    };

    window.smOrgIdentity = {
        slugify: (raw) =>
            raw.toLowerCase()
                .replace(/[^a-z0-9]+/g, "-")
                .replace(/^[0-9-]+/, "")
                .slice(0, 30)
                .replace(/-+$/, ""),

        // check-slug runs the real validator, so the client never mirrors it.
        checkSlug: async (slug) => {
            const res = await fetch(
                `/api/v1/orgs/check-slug?slug=${encodeURIComponent(slug)}`,
                { headers: { Accept: "application/json" } },
            );
            return res.json();
        },

        patch: async (orgId, body) => {
            const res = await fetch(`/api/v1/orgs/${orgId}`, {
                method: "PATCH",
                headers,
                body: JSON.stringify(body),
            });
            if (res.ok) return { ok: true };
            let data = null;
            try { data = await res.json(); } catch { /* not JSON */ }
            return { ok: false, data, status: res.status };
        },
    };
})();
