// Progressive enhancement for the marketing header. Everything degrades to
// plain <details> disclosures when this script is absent: the mobile menu and
// each desktop dropdown stay click-toggleable with native keyboard support.
// Sitewide script, so it also carries the delegated link reporting and the
// per-page engagement beacons.
import "./_link_event.js";
import "./_engagement.js";
import "./_badges.js";
(() => {
    const desktop = matchMedia("(min-width: 768px)");

    // ── Mobile disclosure — close on Escape, outside click, link activation ──
    const menu = document.querySelector(".mk-menu");
    if (menu) {
        const summary = menu.querySelector("summary");
        const close = () => { menu.open = false; };

        document.addEventListener("keydown", (e) => {
            if (e.key === "Escape" && menu.open) {
                close();
                summary.focus();
            }
        });
        document.addEventListener("click", (e) => {
            if (menu.open && !menu.contains(e.target)) close();
        });
        menu.querySelector(".mk-menu__panel").addEventListener("click", (e) => {
            if (e.target.closest("a")) close();
        });
        desktop.addEventListener("change", (e) => { if (e.matches) close(); });
    }

    // ── Desktop dropdowns — hover intent, single-open, keyboard-friendly ──
    const groups = Array.from(document.querySelectorAll(".mk-navgrp"));
    if (!groups.length) return;

    const closeAll = (except) => {
        for (const g of groups) if (g !== except) g.open = false;
    };

    for (const group of groups) {
        let hoverTimer;

        // Only one dropdown open at a time, however it was opened.
        group.addEventListener("toggle", () => {
            if (group.open) closeAll(group);
        });

        // Hover intent is a desktop affordance; touch/mobile falls back to click.
        group.addEventListener("mouseenter", () => {
            if (!desktop.matches) return;
            clearTimeout(hoverTimer);
            group.open = true;
        });
        group.addEventListener("mouseleave", () => {
            if (!desktop.matches) return;
            clearTimeout(hoverTimer);
            hoverTimer = setTimeout(() => { group.open = false; }, 120);
        });

        group.querySelector(".mk-mega").addEventListener("click", (e) => {
            if (e.target.closest("a")) group.open = false;
        });
    }

    document.addEventListener("keydown", (e) => {
        if (e.key !== "Escape") return;
        const open = groups.find((g) => g.open);
        if (!open) return;
        open.open = false;
        open.querySelector("summary").focus();
    });
    document.addEventListener("click", (e) => {
        if (!e.target.closest(".mk-navgrp")) closeAll();
    });
    desktop.addEventListener("change", () => closeAll());
})();
