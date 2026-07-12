// Enhance the header disclosure menu: close on Escape, outside click, link
// activation, and when the viewport grows past the desktop breakpoint. The
// menu is a plain <details> and stays fully usable if this script is absent.
(() => {
    const menu = document.querySelector(".mk-menu");
    if (!menu) return;
    const summary = menu.querySelector("summary");
    const close = () => {
        menu.open = false;
    };

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
    const desktop = matchMedia("(min-width: 768px)");
    desktop.addEventListener("change", (e) => {
        if (e.matches) close();
    });
})();
