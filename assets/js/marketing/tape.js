// Auto-running horizontal tape of check kinds. The cell run is cloned so
// scrolling wraps into identical content; holding the pointer down stops it,
// releasing lets it run again. Without this script the viewport stays a
// plain scroller.
(() => {
    const tape = document.querySelector("[data-tape]");
    if (!tape) return;

    const viewport = tape.querySelector("[data-tape-viewport]");
    const track = tape.querySelector("[data-tape-track]");
    if (!viewport || !track) return;

    const SPEED = 55; // px per second — a card passes every ~6s
    const DRAG_SLOP = 4; // px of movement that turns a click into a drag
    const SETTLE_MS = 600; // smooth-scroll budget; wrapping mid-animation cancels it
    const FLING_MS = 1200; // touch momentum budget before the motor takes back over

    const originals = [...track.children];
    if (!originals.length) return;
    const copyRun = () => {
        for (const card of originals) {
            const clone = card.cloneNode(true);
            clone.setAttribute("aria-hidden", "true");
            track.append(clone);
        }
    };
    copyRun();

    // Distance from a card to its own clone: one full lap, gaps and padding
    // included. Read from layout rather than derived, so CSS stays the source.
    const lap = () => track.children[originals.length].offsetLeft - originals[0].offsetLeft;
    const norm = (x) => {
        const span = lap();
        return span > 0 ? ((x % span) + span) % span : x;
    };

    // A viewport wider than the content trailing one lap would show the track
    // running out at the wrap point, so lay down more copies until it can't.
    while (lap() > 0 && track.scrollWidth - lap() < viewport.clientWidth && track.children.length < originals.length * 6) {
        copyRun();
    }

    const calm = matchMedia("(prefers-reduced-motion: reduce)");
    let pointerHeld = false;
    let last = 0;
    let settleUntil = 0;
    // Own float position. A frame's worth of travel is sub-pixel, and browsers
    // that round scrollLeft would swallow it on every write, so the fraction has
    // to accumulate here rather than in the element.
    let pos = viewport.scrollLeft;
    let applied = pos;

    // :focus-visible rather than focus: a mouse click focuses the tabindex
    // viewport too, and that must not stop the tape — only keyboard focus does.
    const running = () => !calm.matches && !pointerHeld && !viewport.matches(":focus-visible");

    const step = (now) => {
        const dt = last ? Math.min((now - last) / 1000, 0.1) : 0;
        last = now;
        if (now >= settleUntil) {
            if (running()) {
                if (Math.abs(viewport.scrollLeft - applied) > 1) pos = viewport.scrollLeft;
                pos = norm(pos + SPEED * dt);
                viewport.scrollLeft = pos;
                applied = viewport.scrollLeft;
            } else {
                // Stopped: the element owns the position, so only step in to wrap.
                // Writing every frame would kill touch momentum and smooth scroll.
                const wrapped = norm(viewport.scrollLeft);
                if (Math.abs(wrapped - viewport.scrollLeft) > 0.5) viewport.scrollLeft = wrapped;
                pos = viewport.scrollLeft;
            }
        }
        requestAnimationFrame(step);
    };

    const nudge = (dir) => {
        const width = track.children[1].offsetLeft - track.children[0].offsetLeft;
        // Stepping off either end would clamp, so start the step a lap away
        // from the edge — same content, room to travel.
        if (dir < 0 && viewport.scrollLeft < width) viewport.scrollLeft += lap();
        if (dir > 0 && viewport.scrollLeft + width > viewport.scrollWidth - viewport.clientWidth) viewport.scrollLeft -= lap();
        settleUntil = performance.now() + SETTLE_MS;
        viewport.scrollBy({ left: dir * width, behavior: "smooth" });
    };

    viewport.addEventListener("keydown", (e) => {
        if (e.key === "ArrowRight") { e.preventDefault(); nudge(1); }
        else if (e.key === "ArrowLeft") { e.preventDefault(); nudge(-1); }
    });

    // Mouse drags scrub the tape; touch and pen already scroll it natively, so
    // they only park the motor until the fling has died down.
    let dragFrom = null;

    viewport.addEventListener("pointerdown", (e) => {
        if (e.pointerType !== "mouse") { pointerHeld = true; return; }
        if (e.button !== 0) return;
        pointerHeld = true;
        dragFrom = { x: e.clientX, scroll: viewport.scrollLeft };
        settleUntil = 0;
        viewport.setPointerCapture(e.pointerId);
    });

    viewport.addEventListener("pointermove", (e) => {
        if (!dragFrom) return;
        const dx = e.clientX - dragFrom.x;
        if (Math.abs(dx) > DRAG_SLOP) tape.classList.add("is-dragging");
        viewport.scrollLeft = norm(dragFrom.scroll - dx);
    });

    const release = (e) => {
        if (!pointerHeld) return;
        if (e.pointerType !== "mouse") settleUntil = performance.now() + FLING_MS;
        pointerHeld = false;
        dragFrom = null;
        tape.classList.remove("is-dragging");
        if (viewport.hasPointerCapture?.(e.pointerId)) viewport.releasePointerCapture(e.pointerId);
    };
    viewport.addEventListener("pointerup", release);
    viewport.addEventListener("pointercancel", release);

    requestAnimationFrame(step);
})();
