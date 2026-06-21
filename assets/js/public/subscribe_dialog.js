document.addEventListener('click', function (e) {
    if (e.target.closest('[data-subscribe-open]')) {
        const dialog = document.getElementById('subscribe-dialog');
        if (dialog) dialog.showModal();
        return;
    }
    if (e.target.closest('[data-subscribe-close]')) {
        const dialog = document.getElementById('subscribe-dialog');
        if (dialog) dialog.close();
    }
});

document.addEventListener('change', function (e) {
    const radio = e.target.closest('[data-subscribe-channel]');
    if (!radio) return;
    const webhook = radio.value === 'webhook';
    const emailPane = document.querySelector('[data-channel-pane="email"]');
    const webhookPane = document.querySelector('[data-channel-pane="webhook"]');
    if (emailPane) emailPane.hidden = webhook;
    if (webhookPane) webhookPane.hidden = !webhook;
    const email = document.getElementById('subscribe-email');
    const url = document.getElementById('subscribe-url');
    if (email) email.required = !webhook;
    if (url) url.required = webhook;
});
