// Nav org picker: positions the menu via smPositionFloating and switches the
// session's active org through the JSON API. Document-level delegation — the
// picker arrives via an htmx load after this script runs.
(function () {
    let open = null;

    // Picker-own attributes: monitors_list.js delegates on the generic
    // [data-menu-toggle] at document level, so sharing names would make
    // its handler swallow picker clicks and strand open row menus.
    const panelOf = (root) => root.querySelector("[data-picker-panel]");
    const btnOf = (root) => root.querySelector("[data-picker-toggle]");

    function close() {
        if (!open) return;
        const root = open;
        open = null;
        root.dataset.open = "false";
        panelOf(root).hidden = true;
        btnOf(root).setAttribute("aria-expanded", "false");
    }

    function openFor(root) {
        if (open === root) {
            close();
            return;
        }
        close();
        open = root;
        root.dataset.open = "true";
        btnOf(root).setAttribute("aria-expanded", "true");
        window.smPositionFloating(btnOf(root), panelOf(root), {});
    }

    async function switchOrg(item) {
        try {
            const res = await fetch("/api/v1/me/active-org", {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    "X-Requested-With": "uptimepage",
                },
                body: JSON.stringify({ org_id: item.dataset.orgSwitch }),
            });
            if (res.status === 204) {
                // Pages are org-scoped; land on the new org's dashboard.
                window.location.href = "/";
                return;
            }
        } catch {
            /* fall through to reload */
        }
        // Membership revoked or session dead — reload shows server truth.
        window.location.reload();
    }

    document.addEventListener("click", (e) => {
        const toggle = e.target.closest("[data-picker-toggle]");
        if (toggle) {
            e.preventDefault();
            openFor(toggle.closest("[data-org-picker]"));
            return;
        }
        const item = e.target.closest("[data-org-switch]");
        if (item && open && open.contains(item)) {
            e.preventDefault();
            close();
            if (item.getAttribute("aria-current") !== "true") switchOrg(item);
            return;
        }
        if (open && !open.contains(e.target)) close();
    });

    document.addEventListener("keydown", (e) => {
        if (e.key === "Escape" && open) {
            const btn = btnOf(open);
            close();
            btn.focus();
        }
    });

    const reposition = () => {
        if (open) window.smPositionFloating(btnOf(open), panelOf(open), {});
    };
    window.addEventListener("scroll", reposition, true);
    window.addEventListener("resize", reposition);
})();
