document.addEventListener('click', function (e) {
    const btn = e.target.closest('[data-copy]');
    if (!btn) return;
    const target = document.querySelector(btn.getAttribute('data-copy'));
    if (!target) return;
    navigator.clipboard.writeText(target.textContent.trim()).then(function () {
        const label = btn.querySelector('[data-copy-label]') || btn;
        const original = label.textContent;
        label.textContent = 'copied';
        setTimeout(function () { label.textContent = original; }, 1500);
    });
});
