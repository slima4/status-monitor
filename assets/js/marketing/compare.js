// Comparison-page enhancements: the hero config viewer's tabs, and the bottom
// call bar. Both are additive: without this script the config panes stack
// and read in full, and the bar never appears.
(() => {
    const conf = document.querySelector("[data-conf]");
    if (conf) {
        const tabs = [...conf.querySelectorAll("[data-conf-tab]")];
        const panes = [...conf.querySelectorAll("[data-conf-pane]")];

        const show = (id) => {
            for (const tab of tabs) {
                const on = tab.dataset.confTab === id;
                tab.classList.toggle("is-on", on);
                tab.setAttribute("aria-selected", on ? "true" : "false");
                // One tab stop for the widget; the arrows move within it.
                tab.tabIndex = on ? 0 : -1;
            }
            for (const pane of panes) pane.hidden = pane.dataset.confPane !== id;
        };

        for (const tab of tabs) {
            tab.hidden = false;
            tab.addEventListener("click", () => show(tab.dataset.confTab));
        }

        // A tablist answers to the arrow keys; roving focus follows selection.
        conf.addEventListener("keydown", (event) => {
            const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
            if (!step || !event.target.dataset.confTab) return;
            event.preventDefault();
            const at = tabs.indexOf(event.target);
            const next = tabs[(at + step + tabs.length) % tabs.length];
            show(next.dataset.confTab);
            next.focus();
        });

        if (tabs.length) show(tabs[0].dataset.confTab);
    }

    const bar = document.querySelector("[data-cmp-bar]");
    const hero = document.querySelector(".mk-cmp-hero__cta");
    if (!bar || !hero) return;

    // The bar stands in for the hero button once it has scrolled away, and
    // gets out of the way again wherever another one is already on screen.
    const rivals = [hero, ...document.querySelectorAll(".mk-fit__cta, [data-cmp-end]")];
    const onScreen = new Set();
    const watch = new IntersectionObserver((entries) => {
        for (const entry of entries) {
            if (entry.isIntersecting) onScreen.add(entry.target);
            else onScreen.delete(entry.target);
        }
        bar.hidden = onScreen.size > 0;
    });
    for (const rival of rivals) watch.observe(rival);
})();
