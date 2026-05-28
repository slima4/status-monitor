// Renders every <time data-tz datetime="<UTC ISO8601>"> in the visitor's own
// browser timezone. Two modes, chosen per element:
//   data-tz           — relative label for recency glances ("Just now",
//                        "2 mins ago", "Today at 2:30 PM"), absolute date once
//                        older. For summaries where "how recent" is the point.
//   data-tz="exact"   — full local date+time, always ("May 13, 2026, 2:03:22
//                        PM"). For data/audit tables (check results, incident
//                        times, sessions) where "exactly when" is the point and
//                        a drifting relative label would mislead.
// The title tooltip always carries the full local timestamp with zone name.
//
// The server emits a UTC fallback as the element's text, so the page is fully
// readable without JavaScript; this script only upgrades it. Relative labels
// are refreshed on an interval so "2 mins ago" stays honest on long-lived pages.
(function () {
    "use strict";

    var timeFmt, dateFmt, dateYearFmt, exactFmt, fullFmt;
    try {
        timeFmt = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
        dateFmt = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
        dateYearFmt = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", year: "numeric" });
        exactFmt = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "medium" });
        fullFmt = new Intl.DateTimeFormat(undefined, { dateStyle: "full", timeStyle: "long" });
    } catch (_) { /* Intl unavailable: leave server text untouched */ }

    function sameDay(a, b) {
        return a.getFullYear() === b.getFullYear()
            && a.getMonth() === b.getMonth()
            && a.getDate() === b.getDate();
    }

    function relativeLabel(then, now) {
        var elapsedSec = Math.round((now.getTime() - then.getTime()) / 1000);
        // Clock skew or pending writes can put an instant slightly ahead.
        if (elapsedSec < 45) return "Just now";
        var mins = Math.round(elapsedSec / 60);
        if (mins < 60) return mins + (mins === 1 ? " min ago" : " mins ago");
        if (sameDay(then, now)) return "Today at " + timeFmt.format(then);
        var yesterday = new Date(now.getTime());
        yesterday.setDate(now.getDate() - 1);
        if (sameDay(then, yesterday)) return "Yesterday at " + timeFmt.format(then);
        if (then.getFullYear() === now.getFullYear()) return dateFmt.format(then);
        return dateYearFmt.format(then);
    }

    function decorate(root) {
        // fullFmt is the last formatter constructed, so its presence proves the
        // whole try-block (incl. the dateStyle/timeStyle formatters) succeeded.
        if (!fullFmt) return;
        var now = new Date();
        var nodes = (root || document).querySelectorAll("time[data-tz][datetime]");
        for (var i = 0; i < nodes.length; i++) {
            var el = nodes[i];
            var then = new Date(el.getAttribute("datetime"));
            if (isNaN(then.getTime())) continue;
            var full = fullFmt.format(then);
            el.textContent = el.getAttribute("data-tz") === "exact"
                ? exactFmt.format(then)
                : relativeLabel(then, now);
            // Visible relative text loses the precise instant; keep the full
            // local timestamp reachable to assistive tech and on hover.
            el.title = full;
            el.setAttribute("aria-label", full);
        }
    }

    function start() {
        decorate(document);
        // Keep relative labels current without re-fetching the page.
        setInterval(function () { decorate(document); }, 30000);
        if (document.body) {
            // Re-localize the whole document, not just the swap target: htmx
            // OOB swaps land outside the primary target (e.g. the detail
            // page's results rows), so a target-scoped pass would miss them.
            document.body.addEventListener("htmx:afterSettle", function () {
                decorate(document);
            });
        }
    }

    if (document.readyState === "loading") {
        document.addEventListener("DOMContentLoaded", start);
    } else { start(); }
})();
