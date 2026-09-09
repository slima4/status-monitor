// Polled zones marked [data-poll-pause] don't fetch while the tab is
// hidden: a backgrounded tab costs a request per interval per viewer and
// shows the reader nothing. Zones that also listen for sm:poll-resume
// refetch once on return, so what's on screen is current immediately
// rather than one interval later.
//
// Not an hx-trigger filter: htmx compiles those with Function(), which
// the status-page CSP forbids, and it drops a filter it can't compile
// without saying so — the guard would silently do nothing there.
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
})();
