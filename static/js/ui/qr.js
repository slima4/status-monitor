// Shared QR renderer over qrcode.min.js, loaded (deferred) before the
// page scripts. Callers keep their own missing-library policy.
window.smRenderQr = function (el, url, opts) {
    const o = opts || {};
    const qr = qrcode(0, "M");
    qr.addData(url);
    qr.make();
    el.innerHTML = qr.createSvgTag({ cellSize: 4, margin: o.margin ?? 4, scalable: !!o.scalable });
    if (o.size) {
        const svg = el.querySelector("svg");
        if (svg) {
            svg.style.width = `${o.size}px`;
            svg.style.height = `${o.size}px`;
            svg.style.display = "block";
        }
    }
};
