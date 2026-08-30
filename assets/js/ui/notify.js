// App-wide non-blocking notices.
//   smToast({message, kind})                        // bottom-right rail
//   smToast({message, kind, placement: "center"})   // screen-centre notice
//   smToastClear()                                  // drop anything showing
//
// kind: "error" (default) | "ok" | "warn" | "info".

(function () {
    const RAIL_MS = 4000;
    const RAIL_FADE_MS = 200;
    const RAIL_REMOVE_MS = 250;
    const CENTER_HOLD_MS = 2000;
    // Failures have no banner to fall back to, so they hold longer.
    const CENTER_HOLD_ERROR_MS = 5000;
    const CENTER_EXIT_MS = 260;
    const DISMISS_ARM_MS = 450;
    const ANNOUNCE_MS = 6000;

    const RAIL_CLASS = {
        error: "alert-card alert-card--error",
        info:  "alert-card alert-card--muted",
        ok:    "alert-card alert-card--ok",
        warn:  "alert-card alert-card--warn",
    };
    const GLYPH = { ok: "✓", error: "✕", warn: "!", info: "·" };

    let livePolite = null;
    let liveAssertive = null;
    let rail = null;
    let centerLayer = null;

    // The regions have to exist before any message lands in one: a region
    // created and filled in the same task is not announced.
    function mount() {
        if (rail) return;
        livePolite = liveRegion("status", "polite");
        liveAssertive = liveRegion("alert", "assertive");
        rail = document.createElement("div");
        rail.className = "sm-toast-rail";
        // The regions already carry the text; reading the visible copy too
        // would say everything twice.
        rail.setAttribute("aria-hidden", "true");
        centerLayer = document.createElement("div");
        centerLayer.className = "sm-flash-layer";
        centerLayer.setAttribute("aria-hidden", "true");
        // Under an open <dialog> everything outside is painted below the
        // backdrop and made inert; the top layer is the way out of both.
        [livePolite, liveAssertive, rail, centerLayer].forEach((el) => {
            if (typeof el.showPopover === "function") el.popover = "manual";
        });
        document.body.append(livePolite, liveAssertive, rail, centerLayer);
        // A closed popover is display:none, so the regions stay open.
        openLayer(livePolite);
        openLayer(liveAssertive);
    }

    function liveRegion(role, politeness) {
        const el = document.createElement("div");
        el.className = "sr-only";
        el.setAttribute("role", role);
        el.setAttribute("aria-live", politeness);
        // Both roles imply atomic, which re-reads every line still in the
        // region each time a new one lands.
        el.setAttribute("aria-atomic", "false");
        return el;
    }

    // A rejected call must not cost the notice — the flag alone still shows it.
    function popoverTo(el, show) {
        if (!el.popover) return;
        try {
            if (show) el.showPopover(); else el.hidePopover();
        } catch (_) { /* the flag governs display either way */ }
    }

    // `data-open` rather than `:popover-open`, which throws in `matches()`
    // where `showPopover` exists but the selector does not.
    function openLayer(el) {
        if ("open" in el.dataset) return;
        popoverTo(el, true);
        el.dataset.open = "";
    }

    function closeLayer(el) {
        if (!("open" in el.dataset)) return;
        delete el.dataset.open;
        popoverTo(el, false);
    }

    function modalOpen() {
        try { return !!document.querySelector("dialog:modal"); } catch (_) { return false; }
    }

    // The top layer stacks in entry order, so a layer that entered first sits
    // under the dialog. Re-entry toggles display, which on a live region can
    // cost the announcement, so it is spent only when there is one to climb.
    function raiseLayer(el) {
        if (modalOpen() && "open" in el.dataset) closeLayer(el);
        openLayer(el);
    }

    function announce(msg, kind) {
        const el = kind === "error" ? liveAssertive : livePolite;
        raiseLayer(el);
        // One node per message, filled a task later: two raised in the same
        // task would otherwise share a text node and only the last be read.
        const line = document.createElement("div");
        line.timers = [];
        el.appendChild(line);
        line.timers.push(setTimeout(() => { line.textContent = msg; }, 0));
        line.timers.push(setTimeout(() => dropLine(line), ANNOUNCE_MS));
    }

    function dropLine(line) {
        line.timers.forEach(clearTimeout);
        line.remove();
    }

    function clearAnnouncements() {
        [livePolite, liveAssertive].forEach((el) => {
            while (el.firstChild) dropLine(el.firstChild);
        });
    }

    let centerEl = null;
    let centerTimers = [];
    // Set for failures, which have no banner left to re-read them from.
    let centerHolds = false;

    // Moving the view is how someone goes looking for the notice, so those
    // keys must not be what takes it away.
    const KEEP_OPEN = new Set([
        "Shift", "Control", "Alt", "Meta", "Tab", "CapsLock",
        "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight",
        "PageUp", "PageDown", "Home", "End",
    ]);

    function dismissOnKey(e) {
        if (e.repeat || KEEP_OPEN.has(e.key) || centerHolds) return;
        clearCenter();
    }

    function dismissOnPointer(e) {
        // A scrollbar drag lands on the root outside its client box.
        const root = document.documentElement;
        if (e.clientX >= root.clientWidth || e.clientY >= root.clientHeight) return;
        clearCenter();
    }

    function stopDismissWatch() {
        document.removeEventListener("pointerdown", dismissOnPointer, true);
        document.removeEventListener("keydown", dismissOnKey, true);
    }

    function clearCenter() {
        centerTimers.forEach(clearTimeout);
        centerTimers = [];
        centerHolds = false;
        stopDismissWatch();
        if (centerEl) {
            centerEl.remove();
            centerEl = null;
        }
        if (!centerLayer) return;
        closeLayer(centerLayer);
    }

    // The shared formatter, so this follows the account's 12h/24h setting;
    // without Intl the line is dropped rather than shown in a stray format.
    function stamp() {
        return window.smLocalFmt ? window.smLocalFmt.timeSec(new Date()) : "";
    }

    function showCenter(msg, kind) {
        clearCenter();
        const el = document.createElement("div");
        el.className = `sm-flash sm-flash--${kind}`;
        const glyph = document.createElement("span");
        glyph.className = "sm-flash__glyph";
        glyph.setAttribute("aria-hidden", "true");
        glyph.textContent = GLYPH[kind] || GLYPH.info;
        const body = document.createElement("div");
        body.className = "sm-flash__body";
        const text = document.createElement("span");
        text.className = "sm-flash__msg";
        text.textContent = msg;
        body.append(text);
        const at = stamp();
        if (at) {
            const meta = document.createElement("span");
            meta.className = "sm-flash__meta";
            meta.textContent = at;
            body.append(meta);
        }
        el.append(glyph, body);
        openLayer(centerLayer);
        centerLayer.appendChild(el);
        centerEl = el;
        centerHolds = kind === "error";
        const hold = centerHolds ? CENTER_HOLD_ERROR_MS : CENTER_HOLD_MS;
        centerTimers.push(setTimeout(() => {
            el.classList.add("sm-flash--out");
            // Not transitionend: a backgrounded tab may never fire it.
            centerTimers.push(setTimeout(clearCenter, CENTER_EXIT_MS));
        }, hold));
        // Delayed, or the input that raised the notice — including the second
        // half of a double-click — dismisses it.
        centerTimers.push(setTimeout(() => {
            if (centerEl !== el) return;
            document.addEventListener("pointerdown", dismissOnPointer, true);
            document.addEventListener("keydown", dismissOnKey, true);
        }, DISMISS_ARM_MS));
    }

    function dropToast(t) {
        t.timers.forEach(clearTimeout);
        t.remove();
        if (!rail.firstChild) closeLayer(rail);
    }

    function clearRail() {
        while (rail.firstChild) dropToast(rail.firstChild);
    }

    function showRail(msg, kind) {
        const t = document.createElement("div");
        t.className = RAIL_CLASS[kind];
        t.textContent = msg;
        t.timers = [];
        t.addEventListener("click", () => dropToast(t));
        raiseLayer(rail);
        rail.appendChild(t);
        t.timers.push(setTimeout(() => {
            t.style.transition = `opacity ${RAIL_FADE_MS}ms`;
            t.style.opacity = "0";
            t.timers.push(setTimeout(() => dropToast(t), RAIL_REMOVE_MS));
        }, RAIL_MS));
    }

    window.smToast = function (opts) {
        mount();
        opts = opts || {};
        const kind = Object.hasOwn(RAIL_CLASS, opts.kind) ? opts.kind : "error";
        const msg = opts.message || "";
        if (opts.placement === "center") showCenter(msg, kind);
        else showRail(msg, kind);
        announce(msg, kind);
    };

    window.smToastClear = function () {
        if (!rail) return;
        clearCenter();
        clearRail();
        clearAnnouncements();
    };

    if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", mount, { once: true });
    else mount();
})();
