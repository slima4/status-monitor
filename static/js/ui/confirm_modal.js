// Shared confirmation modal. One <dialog> is lazy-mounted to body; every
// "are you sure?" prompt across the app uses it.
//
// Direct call:
//   await smConfirm({title, body, confirmLabel, danger}) -> boolean
//
// Declarative wiring (works for hx-delete, vanilla onclick, fetch buttons):
//   <button hx-delete="/..." data-confirm-modal
//           data-confirm-title="Delete?" data-confirm-body="Cannot undo."
//           data-confirm-label="Delete" data-confirm-danger>...
//
// The click-capture interceptor stops the original event, shows the modal,
// and only re-dispatches the original click when the user confirms. The
// `armed` dataset flag keeps the re-dispatched click from re-triggering us.

(function () {
    let dialog = null;
    let titleEl, bodyEl, confirmBtn, cancelBtn;
    let resolveFn = null;

    function mount() {
        if (dialog) return;
        dialog = document.createElement("dialog");
        dialog.id = "sm-confirm-modal";
        dialog.className = "w-full max-w-md rounded-lg p-0 backdrop:bg-slate-900/40";
        dialog.setAttribute("aria-labelledby", "sm-confirm-title");
        dialog.innerHTML =
            '<div class="space-y-4 p-6">' +
                '<h2 id="sm-confirm-title" class="text-lg font-semibold"></h2>' +
                '<p class="text-sm text-slate-600"></p>' +
                '<div class="flex items-center justify-end gap-2 pt-2">' +
                    '<button type="button" data-sm-confirm-cancel ' +
                            'class="btn-ghost px-3 py-1.5 text-sm font-medium text-slate-700"></button>' +
                    '<button type="button" data-sm-confirm-ok ' +
                            'class="px-3 py-1.5 text-sm font-medium"></button>' +
                '</div>' +
            '</div>';
        document.body.appendChild(dialog);
        titleEl    = dialog.querySelector("h2");
        bodyEl     = dialog.querySelector("p");
        confirmBtn = dialog.querySelector("[data-sm-confirm-ok]");
        cancelBtn  = dialog.querySelector("[data-sm-confirm-cancel]");

        confirmBtn.addEventListener("click", () => settle(true));
        cancelBtn.addEventListener("click",  () => settle(false));
        // Backdrop click cancels.
        dialog.addEventListener("click", (e) => {
            if (e.target === dialog) settle(false);
        });
        // ESC fires `cancel` (preventable; native default is to close).
        dialog.addEventListener("cancel", () => settle(false));
    }

    function settle(ok) {
        if (!resolveFn) return;
        const r = resolveFn;
        resolveFn = null;
        if (dialog && dialog.open) dialog.close();
        r(ok);
    }

    window.smConfirm = function (opts) {
        mount();
        opts = opts || {};
        titleEl.textContent    = opts.title        || "Are you sure?";
        bodyEl.textContent     = opts.body         || "";
        confirmBtn.textContent = opts.confirmLabel || "Confirm";
        cancelBtn.textContent  = opts.cancelLabel  || "Cancel";

        const isDanger = !!opts.danger;
        titleEl.className = "text-lg font-semibold " +
            (isDanger ? "text-rose-700" : "text-slate-800");
        confirmBtn.className = "sticker-btn px-3 py-1.5 text-sm font-medium " +
            (isDanger ? "sticker-btn--danger" : "sticker-btn--primary");

        // Re-entrance: a prior modal still open (e.g. caller didn't await).
        // Settle it false and close before re-opening, or showModal throws
        // InvalidStateError on an already-open dialog.
        if (dialog.open) {
            settle(false);
        }

        return new Promise((resolve) => {
            resolveFn = resolve;
            dialog.showModal();
            // Pre-focus the safer button for destructive actions.
            (isDanger ? cancelBtn : confirmBtn).focus();
        });
    };

    // Capture-phase click interceptor: runs before htmx (bubble-phase) and
    // before any per-page click handlers, so we can stop and replace the
    // browser's native confirm() behavior uniformly.
    document.addEventListener("click", function (e) {
        const trigger = e.target.closest("[data-confirm-modal]");
        if (!trigger || trigger.dataset.confirmModalArmed === "1") return;
        e.preventDefault();
        e.stopImmediatePropagation();
        const d = trigger.dataset;
        window.smConfirm({
            title:        d.confirmTitle || "Are you sure?",
            body:         d.confirmBody  || "",
            confirmLabel: d.confirmLabel || "Confirm",
            cancelLabel:  d.confirmCancel,
            danger:       d.confirmDanger !== undefined,
        }).then(function (ok) {
            if (!ok) return;
            trigger.dataset.confirmModalArmed = "1";
            try { trigger.click(); } finally {
                queueMicrotask(() => { delete trigger.dataset.confirmModalArmed; });
            }
        });
    }, true);
})();
