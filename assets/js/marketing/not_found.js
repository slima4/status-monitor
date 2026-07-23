// Report the missing path so broken inbound links surface in analytics.
window.umami?.track("not-found", { path: location.pathname, ref: document.referrer });
