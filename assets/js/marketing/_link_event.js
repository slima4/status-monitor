// Nav, footer and outbound clicks. One delegated listener rather than an
// attribute on each of ~60 links, and Umami's own handler ignores anything
// without data-umami-event, so outbound needs reporting here too.
const site = location.hostname.split(".").slice(-2).join(".");

const ours = (host) => host === site || host.endsWith(`.${site}`);

document.addEventListener("click", (e) => {
    const link = e.target.closest("a[href]");
    // Tagged links are Umami's to report; reporting them again double-counts.
    if (!link || link.hasAttribute("data-umami-event")) return;

    let url;
    try {
        url = new URL(link.href, location.href);
    } catch {
        return;
    }
    if (!url.protocol.startsWith("http")) return;

    if (!ours(url.hostname)) {
        window.umami?.track("outbound", { to: url.hostname + url.pathname });
        return;
    }
    const region = link.closest("[data-link-region]")?.dataset.linkRegion;
    if (region) window.umami?.track("nav-click", { region, to: url.pathname + url.hash });
});
