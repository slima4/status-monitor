// Auto-loaded list partials (marked [data-hx-loader]) sit on their "Loading…"
// placeholder forever when the fetch fails — htmx leaves the target untouched
// on a 4xx/5xx or network error. Swap in an inline error with a retry instead,
// but only while the placeholder is still up: see onError.
(function () {
    // Only the loader's OWN load/poll request — never a nested action (a
    // row delete, an inline form) whose failure must not wipe the list.
    function loaderFor(evt) {
        const elt = evt.detail && evt.detail.elt;
        return elt && elt.matches && elt.matches("[data-hx-loader]") ? elt : null;
    }

    // Mirrors the `reg::loading` Askama macro — keep both in step.
    function loadingPlaceholder() {
        const p = Object.assign(document.createElement("p"), {
            className: "px-4 py-3 font-mono text-xs text-quiet",
            textContent: "# loading…",
        });
        p.setAttribute("data-hx-loading", "");
        return p;
    }

    function showError(box) {
        const url = box.getAttribute("hx-get");
        box.replaceChildren();
        const card = document.createElement("p");
        card.className = "sticker-card px-4 py-3 text-sm text-muted";
        card.setAttribute("role", "alert");
        card.append("couldn't load this list. ");
        const retry = document.createElement("button");
        retry.type = "button";
        retry.className = "row-link text-sm";
        retry.textContent = "retry";
        retry.addEventListener("click", () => {
            box.replaceChildren(loadingPlaceholder());
            window.htmx.ajax("GET", url, { target: box, swap: "innerHTML" });
        });
        card.appendChild(retry);
        box.appendChild(card);
    }

    // Only a box still showing its placeholder. A poll or a refresh that
    // fails once the list has rendered leaves it alone — stale rows beat an
    // error card that throws away what the reader was looking at.
    function onError(evt) {
        const box = loaderFor(evt);
        if (box && box.querySelector("[data-hx-loading]")) showError(box);
    }

    document.body.addEventListener("htmx:responseError", onError);
    document.body.addEventListener("htmx:sendError", onError);
    document.body.addEventListener("htmx:timeout", onError);
})();
