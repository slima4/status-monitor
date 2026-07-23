import { addCopyButtons } from "./_copy_code.js";

// Progressive enhancement for a documentation page: copy buttons on code
// blocks and an on-page contents list that tracks the heading you are
// reading. Without this script the page keeps its code, its anchors, and a
// working contents list.
(() => {
    const body = document.querySelector(".mk-doc-body");
    if (!body) return;

    addCopyButtons(body);

    const links = new Map();
    for (const a of document.querySelectorAll(".mk-doc-toc a")) {
        const id = decodeURIComponent(a.hash.slice(1));
        if (id) links.set(id, a);
    }
    const headings = [...links.keys()]
        .map((id) => document.getElementById(id))
        .filter(Boolean);
    if (!headings.length) return;

    let active;
    const mark = (id) => {
        if (active === id) return;
        links.get(active)?.classList.remove("is-active");
        links.get(id)?.classList.add("is-active");
        active = id;
    };

    // Top band only, so the marked entry is the heading you are under rather
    // than whichever one happens to be visible at the bottom of the viewport.
    const observer = new IntersectionObserver(
        (entries) => {
            const visible = entries
                .filter((e) => e.isIntersecting)
                .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
            if (visible.length) mark(visible[0].target.id);
        },
        { rootMargin: "-88px 0px -75% 0px" },
    );
    for (const h of headings) observer.observe(h);
})();
