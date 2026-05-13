// Decorates every <time data-tz datetime="..."> element with a tooltip
// (title + aria-label) showing the same instant in the visitor's local
// timezone. Visible text stays as the server-emitted UTC string so the
// page is fully readable without JavaScript and the no-JS rendering does
// not flicker when the script runs.
(function () {
    "use strict";
    function tzName() {
        try { return Intl.DateTimeFormat().resolvedOptions().timeZone || "local"; }
        catch (_) { return "local"; }
    }
    function fmtLocal(d) {
        try {
            return new Intl.DateTimeFormat(undefined, {
                year: "numeric", month: "2-digit", day: "2-digit",
                hour: "2-digit", minute: "2-digit", hour12: false,
            }).format(d);
        } catch (_) { return d.toString(); }
    }
    function decorate(root) {
        var zone = tzName();
        var nodes = (root || document).querySelectorAll("time[data-tz][datetime]");
        for (var i = 0; i < nodes.length; i++) {
            var el = nodes[i];
            var d = new Date(el.getAttribute("datetime"));
            if (isNaN(d.getTime())) continue;
            var local = fmtLocal(d);
            el.title = "Local (" + zone + "): " + local;
            el.setAttribute("aria-label", el.textContent + " — local " + zone + ": " + local);
        }
    }
    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", function () { decorate(document); });
    } else { decorate(document); }
    document.body && document.body.addEventListener("htmx:afterSwap", function (e) {
        decorate(e.detail && e.detail.target);
    });
})();
