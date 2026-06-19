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
