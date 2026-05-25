// Shared user-dialog helpers. One <dialog> is lazy-mounted to body and
// reused for every modal interaction across the app; a separate toast
// rail handles non-blocking error/info notices.
//
// Direct calls:
//   await smConfirm({title, body, confirmLabel, danger}) -> boolean
//   await smPrompt({title, body, placeholder, value, splitOnComma}) -> string | string[] | null
//   smToast({message, kind})           // kind: "error" (default) | "info" | "ok"
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

    // smPrompt: trimmed string, array when splitOnComma, or null.
    let promptDialog = null;
    let promptTitleEl, promptBodyEl, promptInputEl, promptOkBtn, promptCancelBtn;
    let promptResolve = null;

    function mountPrompt() {
        if (promptDialog) return;
        promptDialog = document.createElement("dialog");
        promptDialog.id = "sm-prompt-modal";
        promptDialog.className = "w-full max-w-md rounded-lg p-0 backdrop:bg-slate-900/40";
        promptDialog.setAttribute("aria-labelledby", "sm-prompt-title");
        promptDialog.innerHTML =
            '<form method="dialog" class="space-y-4 p-6">' +
                '<h2 id="sm-prompt-title" class="text-lg font-semibold text-slate-800"></h2>' +
                '<p class="text-sm text-slate-600"></p>' +
                '<input type="text" class="field w-full text-sm" data-sm-prompt-input>' +
                '<div class="flex items-center justify-end gap-2 pt-2">' +
                    '<button type="button" data-sm-prompt-cancel ' +
                            'class="btn-ghost px-3 py-1.5 text-sm font-medium text-slate-700">Cancel</button>' +
                    '<button type="submit" data-sm-prompt-ok ' +
                            'class="sticker-btn sticker-btn--primary px-3 py-1.5 text-sm font-medium">OK</button>' +
                '</div>' +
            '</form>';
        document.body.appendChild(promptDialog);
        promptTitleEl  = promptDialog.querySelector("h2");
        promptBodyEl   = promptDialog.querySelector("p");
        promptInputEl  = promptDialog.querySelector("[data-sm-prompt-input]");
        promptOkBtn    = promptDialog.querySelector("[data-sm-prompt-ok]");
        promptCancelBtn= promptDialog.querySelector("[data-sm-prompt-cancel]");

        promptDialog.querySelector("form").addEventListener("submit", (e) => {
            e.preventDefault();
            settlePrompt(promptInputEl.value);
        });
        promptCancelBtn.addEventListener("click", () => settlePrompt(null));
        promptDialog.addEventListener("click", (e) => {
            if (e.target === promptDialog) settlePrompt(null);
        });
        promptDialog.addEventListener("cancel", (e) => {
            e.preventDefault();
            settlePrompt(null);
        });
    }
    function settlePrompt(raw) {
        if (!promptResolve) return;
        const r = promptResolve;
        promptResolve = null;
        const split = promptDialog.dataset.split === "1";
        if (promptDialog.open) promptDialog.close();
        if (raw === null) { r(null); return; }
        const trimmed = String(raw).trim();
        if (split) {
            r(trimmed.split(",").map(s => s.trim()).filter(Boolean));
        } else {
            r(trimmed === "" ? null : trimmed);
        }
    }
    window.smPrompt = function (opts) {
        mountPrompt();
        opts = opts || {};
        promptTitleEl.textContent  = opts.title       || "Input";
        promptBodyEl.textContent   = opts.body        || "";
        promptInputEl.placeholder  = opts.placeholder || "";
        promptInputEl.value        = opts.value       || "";
        promptDialog.dataset.split = opts.splitOnComma ? "1" : "0";
        if (promptDialog.open) settlePrompt(null);
        return new Promise((resolve) => {
            promptResolve = resolve;
            promptDialog.showModal();
            promptInputEl.focus();
            promptInputEl.select();
        });
    };

    // smToast: bottom-right rail, click or 4s auto-dismiss.
    let toastRail = null;
    function mountToastRail() {
        if (toastRail) return;
        toastRail = document.createElement("div");
        toastRail.id = "sm-toast-rail";
        toastRail.className = "pointer-events-none fixed bottom-4 right-4 z-50 flex flex-col gap-2";
        document.body.appendChild(toastRail);
    }
    window.smToast = function (opts) {
        mountToastRail();
        opts = opts || {};
        const msg  = opts.message || "";
        const kind = opts.kind    || "error";
        const klass = {
            error: "alert-card alert-card--error",
            info:  "alert-card alert-card--muted",
            ok:    "alert-card alert-card--ok",
            warn:  "alert-card alert-card--warn",
        }[kind] || "alert-card alert-card--error";
        const t = document.createElement("div");
        t.className = "pointer-events-auto cursor-pointer " + klass;
        t.style.minWidth = "16rem";
        t.style.maxWidth = "22rem";
        t.textContent = msg;
        t.addEventListener("click", () => t.remove());
        toastRail.appendChild(t);
        setTimeout(() => {
            t.style.transition = "opacity 200ms";
            t.style.opacity = "0";
            setTimeout(() => t.remove(), 250);
        }, 4000);
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
