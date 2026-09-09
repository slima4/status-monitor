// Lifecycle of the self-rearming polled zones.
//
// [data-poll-pause] zones don't fetch while the tab is hidden: a backgrounded
// tab costs a request per interval per viewer and shows the reader nothing.
// Zones that also listen for sm:poll-resume refetch once on return.
//
// [data-reload-on-404] zones reload once their own endpoint is gone — revoked
// share token, deleted monitor, unpublished page.
//
// Not an hx-trigger filter or an hx-on attribute: htmx compiles both with
// Function(), which the status-page CSP forbids, and drops a filter it can't
// compile without saying so.
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

    // The whole page is stale, not just this zone: the charts beside it don't
    // poll, so they'd sit on a deleted monitor forever. The server already
    // renders the right page. The pause guard keeps this to visible tabs.
    document.body.addEventListener("htmx:responseError", (ev) => {
        if (ev.detail?.xhr?.status !== 404) return;
        if (ev.detail.elt?.matches?.("[data-reload-on-404]")) location.reload();
    });
})();
