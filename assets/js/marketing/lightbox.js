// Arrow-key + Escape nav for the :target lightbox. Click/prev/next work in
// pure CSS; this only adds keyboard stepping and degrades to nothing if absent.
(() => {
    const ids = [...document.querySelectorAll(".mk-lightbox")]
        .map((el) => el.id)
        .filter(Boolean);
    if (ids.length < 2) return;

    addEventListener("keydown", (e) => {
        const i = ids.indexOf(location.hash.slice(1));
        if (i === -1) return;
        if (e.key === "ArrowRight") {
            e.preventDefault();
            location.hash = ids[(i + 1) % ids.length];
        } else if (e.key === "ArrowLeft") {
            e.preventDefault();
            location.hash = ids[(i - 1 + ids.length) % ids.length];
        } else if (e.key === "Escape") {
            location.hash = "see";
        }
    });
})();
