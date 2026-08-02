// Auth URLs carry single-use credentials (magic-link and invitation tokens) and
// Umami stores the query string on every send. Origins survive so the referrer
// still attributes marketing traffic.
const path = (value) => {
    try {
        const url = new URL(value, location.origin);
        return url.origin === location.origin ? url.pathname : url.origin + url.pathname;
    } catch {
        return "";
    }
};

window.smScrubAuthUrl = (_type, payload) => {
    payload.url = path(payload.url);
    if (payload.referrer) payload.referrer = path(payload.referrer);
    return payload;
};
