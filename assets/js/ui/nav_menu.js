// One controller for every [data-navmenu] root (org switcher, account
// dropdown, compact hamburger), one panel open at a time. Own attribute
// namespace because monitors_list.js owns [data-menu-toggle] for row menus.
(function () {
    let open = null;

    const panelOf = (root) => root.querySelector("[data-navmenu-panel]");
    const btnOf = (root) => root.querySelector("[data-navmenu-toggle]");

    function place(root) {
        // Anchor to the rail, not the toggle, so panel edges line up; the 1px
        // overlap reads as the rail extending. Menus outside a rail keep the gap.
        const rail = root.closest(".nav-rail");
        window.smPositionFloating(rail || btnOf(root), panelOf(root), {
            align: root.dataset.navmenuAlign || "start",
            gap: rail ? -1 : undefined,
        });
    }

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
        place(root);
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
        const toggle = e.target.closest("[data-navmenu-toggle]");
        if (toggle) {
            e.preventDefault();
            openFor(toggle.closest("[data-navmenu]"));
            return;
        }
        const org = e.target.closest("[data-org-switch]");
        if (org && open && open.contains(org)) {
            e.preventDefault();
            close();
            if (org.getAttribute("aria-current") !== "true") switchOrg(org);
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
        if (open) place(open);
    };
    window.addEventListener("scroll", reposition, true);
    window.addEventListener("resize", reposition);
})();
