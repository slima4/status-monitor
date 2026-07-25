// Auto-running horizontal tape of check kinds. The cell run is cloned so
// scrolling wraps into identical content; holding the pointer down stops it,
// releasing lets it run again. Clicking a cell flips it to a docs-side back.
// Without this script the viewport stays a plain scroller of front faces.
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
            // No phantom tab stops inside aria-hidden nodes.
            for (const el of clone.querySelectorAll("a, button")) el.tabIndex = -1;
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
    // Index within the original run of the cell showing its back.
    let flippedIdx = null;
    // Own float position. A frame's worth of travel is sub-pixel, and browsers
    // that round scrollLeft would swallow it on every write, so the fraction has
    // to accumulate here rather than in the element.
    let pos = viewport.scrollLeft;
    let applied = pos;

    // :focus-visible rather than focus: a mouse click focuses the tabindex
    // viewport too, and that must not stop the tape — only keyboard focus
    // does, whether on the viewport itself or on a cell link inside it.
    const running = () => !calm.matches && !pointerHeld && flippedIdx === null
        && !viewport.matches(":focus-visible") && !viewport.querySelector(":focus-visible");

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

    // Every copy of a cell flips in step so the wrap seam never shows a
    // split pair.
    const applyFlip = (idx, on) => {
        const cells = track.children;
        for (let k = idx; k < cells.length; k += originals.length) {
            cells[k].classList.toggle("is-flipped", on);
            cells[k].querySelector("[data-tape-flip]").setAttribute("aria-expanded", String(on));
        }
    };
    const unflip = () => {
        if (flippedIdx === null) return;
        applyFlip(flippedIdx, false);
        flippedIdx = null;
    };

    viewport.addEventListener("click", (e) => {
        const btn = e.target.closest("[data-tape-flip]");
        if (!btn) return;
        const raw = [...track.children].indexOf(btn.closest(".mk-tape__cell"));
        const idx = raw % originals.length;
        const opening = flippedIdx !== idx;
        // A clone's button can take mouse focus; shed it before its face
        // hides, or the focusout unflip snaps the card straight back.
        if (raw >= originals.length && document.activeElement === btn) btn.blur();
        unflip();
        if (opening) { flippedIdx = idx; applyFlip(idx, true); }
        // The pressed button hides with its face; unhanded, focus drops to body.
        if (raw < originals.length && document.activeElement === btn) {
            const cell = originals[idx];
            (opening ? cell.querySelector(".mk-tape__docs") : cell.querySelector("[data-tape-flip]"))
                .focus({ preventScroll: true });
        }
    });

    // A card left flipped would park the motor for good, so leaving the
    // tape flips it home.
    document.addEventListener("click", (e) => {
        if (!tape.contains(e.target)) unflip();
    });
    viewport.addEventListener("focusout", (e) => {
        if (!viewport.contains(e.relatedTarget)) unflip();
    });

    viewport.addEventListener("keydown", (e) => {
        if (e.key === "ArrowRight") { e.preventDefault(); nudge(1); }
        else if (e.key === "ArrowLeft") { e.preventDefault(); nudge(-1); }
        else if (e.key === "Escape" && flippedIdx !== null) {
            const cell = originals[flippedIdx];
            const hadFocus = cell.contains(document.activeElement);
            unflip();
            if (hadFocus) cell.querySelector("[data-tape-flip]").focus({ preventScroll: true });
        }
    });

    // Mouse drags scrub the tape; touch and pen already scroll it natively, so
    // they only park the motor until the fling has died down.
    let dragFrom = null;
    let suppressClick = false;

    viewport.addEventListener("pointerdown", (e) => {
        if (e.pointerType !== "mouse") { pointerHeld = true; return; }
        if (e.button !== 0) return;
        pointerHeld = true;
        suppressClick = false;
        dragFrom = { x: e.clientX, scroll: viewport.scrollLeft, id: e.pointerId, live: false };
        settleUntil = 0;
    });

    viewport.addEventListener("pointermove", (e) => {
        if (!dragFrom) return;
        const dx = e.clientX - dragFrom.x;
        // Capture only once real dragging starts: capturing on press would
        // retarget the eventual click away from the cell controls.
        if (!dragFrom.live && Math.abs(dx) > DRAG_SLOP) {
            dragFrom.live = true;
            tape.classList.add("is-dragging");
            viewport.setPointerCapture(dragFrom.id);
        }
        if (dragFrom.live) viewport.scrollLeft = norm(dragFrom.scroll - dx);
    });

    const release = (e) => {
        if (!pointerHeld) return;
        if (e.pointerType !== "mouse") settleUntil = performance.now() + FLING_MS;
        // Only a completed press emits a click; pointercancel never does, and
        // a suppress armed for it would swallow the next real click instead.
        suppressClick = e.type === "pointerup" && dragFrom !== null && dragFrom.live;
        pointerHeld = false;
        dragFrom = null;
        tape.classList.remove("is-dragging");
        if (viewport.hasPointerCapture?.(e.pointerId)) viewport.releasePointerCapture(e.pointerId);
    };
    // On window: capture is deferred until the drag goes live, so a press
    // can end off-viewport; missing that release would park the motor.
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);

    // A drag that ends over a cell must not flip it, navigate, or count as a
    // click for analytics; native link-dragging would hijack the scrub outright.
    viewport.addEventListener("click", (e) => {
        if (suppressClick) { suppressClick = false; e.preventDefault(); e.stopPropagation(); }
    }, true);
    viewport.addEventListener("dragstart", (e) => e.preventDefault());

    requestAnimationFrame(step);
})();
