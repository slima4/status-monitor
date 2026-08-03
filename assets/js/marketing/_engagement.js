// Umami derives time-on-page from the gap between consecutive pageviews, so a
// session that reads one page and leaves reports nothing at all — and most
// sessions here are exactly that. Scroll milestones report as they happen, one
// summary reports on exit, and CTA impressions turn a zero-click placement into
// a readable number instead of an ambiguous one.
const THRESHOLDS = [25, 50, 75, 100];
const page = location.pathname;

const track = (name, props) => window.umami?.track(name, { page, ...props });

// ── Scroll depth ──────────────────────────────────────────────────────────
const reached = new Set();
let deepest = 0;

const depth = () => {
    const total = document.documentElement.scrollHeight;
    // A page that fits the viewport is fully seen the moment it opens.
    if (total <= window.innerHeight) return 100;
    return Math.min(100, Math.round(((window.scrollY + window.innerHeight) / total) * 100));
};

const mark = () => {
    deepest = Math.max(deepest, depth());
    for (const t of THRESHOLDS) {
        if (deepest >= t && !reached.has(t)) {
            reached.add(t);
            track("page-depth", { pct: t });
        }
    }
};

let pending = false;
addEventListener(
    "scroll",
    () => {
        if (pending) return;
        pending = true;
        requestAnimationFrame(() => {
            pending = false;
            mark();
        });
    },
    { passive: true },
);
addEventListener("resize", mark, { passive: true });
mark();

// ── Exit summary ──────────────────────────────────────────────────────────
// Only foreground time counts; a tab left open overnight is not a long read.
let visibleMs = 0;
let shownAt = document.visibilityState === "visible" ? performance.now() : 0;
let summarised = false;

const summarise = () => {
    if (summarised) return;
    summarised = true;
    if (shownAt) visibleMs += performance.now() - shownAt;
    track("page-read", { pct: deepest, secs: Math.round(visibleMs / 1000) });
};

// First hide is the last reliable moment to send. A reader who returns and
// scrolls further goes unreported; the alternative is losing the event outright
// on mobile, where no unload handler is guaranteed to run.
document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "hidden") {
        shownAt = performance.now();
        return;
    }
    summarise();
});
addEventListener("pagehide", summarise);

// ── CTA impressions ───────────────────────────────────────────────────────
// A placement with no clicks is unreadable on its own: nobody scrolled to it
// and everybody ignored it look identical. One view per placement per pageview.
const viewed = new Set();
const observer = new IntersectionObserver(
    (entries) => {
        for (const entry of entries) {
            if (!entry.isIntersecting) continue;
            observer.unobserve(entry.target);
            const position = entry.target.dataset.umamiEventPosition;
            if (!position || viewed.has(position)) continue;
            viewed.add(position);
            track("cta-view", { position });
        }
    },
    { threshold: 0.5 },
);
for (const cta of document.querySelectorAll('[data-umami-event="signup-start"]')) {
    observer.observe(cta);
}
