// Lifecycle of the self-rearming polled zones.
//
// [data-poll-pause] zones don't fetch while the tab is hidden: a
// backgrounded tab costs a request per interval per viewer and shows the
// reader nothing. Zones that also listen for sm:poll-resume refetch once on
// return, so what's on screen is current immediately rather than one
// interval later.
//
// [data-poll-stop-on-404] zones stop for good once their own endpoint is
// gone — a revoked share token, a deleted monitor, an unpublished status
// page — instead of asking every interval for the rest of the tab's life.
//
// None of this is an hx-trigger filter or an hx-on attribute: htmx compiles
// both with Function(), which the status-page CSP forbids, and it drops a
// filter it can't compile without saying so.
(function () {
    document.body.addEventListener("htmx:beforeRequest", (ev) => {
        if (!document.hidden) return;
        const elt = ev.detail && ev.detail.elt;
        if (elt && elt.matches && elt.matches("[data-poll-pause]")) {
            ev.preventDefault();
        }
    });

    document.addEventListener("visibilitychange", () => {
        if (!document.hidden && window.htmx) {
            window.htmx.trigger("body", "sm:poll-resume");
        }
    });

    // Dropping hx-trigger alone leaves the zone polling: htmx read the
    // attribute once and holds the timer and the from:body listeners in the
    // element's internal data. Clearing the verb too and reprocessing is what
    // makes htmx tear those down.
    document.body.addEventListener("htmx:responseError", (ev) => {
        if (ev.detail?.xhr?.status !== 404) return;
        const elt = ev.detail.elt;
        if (!elt || !elt.matches || !elt.matches("[data-poll-stop-on-404]")) return;
        elt.removeAttribute("hx-get");
        elt.removeAttribute("hx-trigger");
        window.htmx.process(elt);
    });
})();
