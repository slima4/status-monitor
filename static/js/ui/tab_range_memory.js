// Per-tab range memory for the monitor / incidents sub-tab strip.
// Each tab has its own range key set, so a single shared value would
// drop user intent on switch. Persist each tab's range in localStorage
// keyed by (target_id, sub_tab) and rewrite the cross-tab anchor on
// load so that flipping tabs preserves the prior range pick.

(function () {
    const targetMatch = location.pathname.match(/\/targets\/([0-9a-f-]+)/i);
    if (!targetMatch) return;
    const targetId = targetMatch[1];
    const isIncidents = /\/incidents(\/|$)/.test(location.pathname);
    const myKey = isIncidents ? "incidents" : "monitor";
    const otherKey = isIncidents ? "monitor" : "incidents";

    const myStoreKey = `smRange:${targetId}:${myKey}`;
    const otherStoreKey = `smRange:${targetId}:${otherKey}`;

    const params = new URLSearchParams(location.search);
    const currentRange = params.get("range");
    if (currentRange) {
        try { localStorage.setItem(myStoreKey, currentRange); } catch (_) {}
    }

    let storedOther = null;
    try { storedOther = localStorage.getItem(otherStoreKey); } catch (_) {}
    if (!storedOther) return;

    const otherPath = isIncidents
        ? `/targets/${targetId}`
        : `/targets/${targetId}/incidents`;
    document.querySelectorAll(`a[href^="${otherPath}"]`).forEach((a) => {
        let url;
        try {
            url = new URL(a.getAttribute("href"), location.origin);
        } catch (_) {
            return;
        }
        // Only the literal cross-tab path; don't touch sub-routes.
        if (url.pathname !== otherPath) return;
        if (url.searchParams.has("range")) return;
        url.searchParams.set("range", storedOther);
        a.setAttribute("href", url.pathname + url.search);
    });
})();
