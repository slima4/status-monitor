// Generic collapsible: a [data-collapse-toggle] inside a [data-collapsible]
// scope flips the region named by the scope's aria-controls toggle.
(function () {
    function toggle(scope) {
        const ctrl = scope.querySelector("[data-collapse-toggle][aria-controls]");
        const body = ctrl && document.getElementById(ctrl.getAttribute("aria-controls"));
        if (!body) return;
        const expanded = body.classList.toggle("hidden") === false;
        ctrl.setAttribute("aria-expanded", String(expanded));
        scope.querySelector("[data-collapse-chevron]")?.classList.toggle("rotate-180", expanded);
        scope.querySelector("[data-collapse-hint]")?.classList.toggle("hidden", expanded);
    }

    document.addEventListener("click", (evt) => {
        const t = evt.target.closest("[data-collapse-toggle]");
        if (!t) return;
        const scope = t.closest("[data-collapsible]");
        if (scope) toggle(scope);
    });

    // Expand the collapsible enclosing `el` (no-op if already open) so a field
    // focused by validation isn't left hidden inside a collapsed section.
    window.smRevealCollapsibleFor = (el) => {
        const scope = el?.closest("[data-collapsible]");
        if (!scope) return;
        const ctrl = scope.querySelector("[data-collapse-toggle][aria-controls]");
        const body = ctrl && document.getElementById(ctrl.getAttribute("aria-controls"));
        if (body && body.classList.contains("hidden")) toggle(scope);
    };
})();
