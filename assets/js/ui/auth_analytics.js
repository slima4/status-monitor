// Auth outcomes with no click to hang a data-umami-event attribute on.
addEventListener("load", () => {
    const el = document.querySelector("[data-auth-event]");
    if (!el) return;
    const props = {};
    if (el.dataset.authEventReason) props.reason = el.dataset.authEventReason;
    window.umami?.track(el.dataset.authEvent, props);
});
