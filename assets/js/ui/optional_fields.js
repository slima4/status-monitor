// Reveal-on-demand chips for the rarely-used HTTP fields (body match, request
// body). The server renders the initial open state; this only toggles on click.
(function () {
    const form = document.getElementById("check-form");
    if (!form) return;

    const toggles = [...form.querySelectorAll("[data-optional-toggle]")];

    const fieldFor = (el) =>
        form.querySelector(`[data-optional-field='${el.dataset.optionalToggle}']`);

    function set(btn, field, open) {
        field.classList.toggle("hidden", !open);
        btn.setAttribute("aria-expanded", String(open));
        if (open) field.querySelector("input, textarea")?.focus();
    }

    toggles.forEach((btn) => {
        const field = fieldFor(btn);
        if (!field) return;
        btn.addEventListener("click", () =>
            set(btn, field, btn.getAttribute("aria-expanded") !== "true"));
    });

    // Submit-time validation focuses an invalid field; if it lives in a
    // collapsed block, open that block first so the focus is visible.
    window.smRevealOptionalFor = (el) => {
        const field = el?.closest("[data-optional-field]");
        if (!field) return;
        const btn = toggles.find((b) => b.dataset.optionalToggle === field.dataset.optionalField);
        if (btn && btn.getAttribute("aria-expanded") !== "true") set(btn, field, true);
    };
})();
