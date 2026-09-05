// Shared user-dialog helpers. One <dialog> is lazy-mounted to body and
// reused for every modal interaction across the app. Non-blocking notices
// live in notify.js (`smToast`).
//
// Direct calls:
//   await smConfirm({title, body, confirmLabel, danger, match}) -> boolean
//     `match` holds confirm disabled until the name is typed back, with a
//     copy button so it stays a speed bump rather than a memory test.
//   await smPrompt({title, body, placeholder, value, splitOnComma}) -> string | string[] | null
//
// Declarative wiring (works for hx-delete, vanilla onclick, fetch buttons):
//   <button hx-delete="/..." data-confirm-modal
//           data-confirm-title="Delete?" data-confirm-body="Cannot undo."
//           data-confirm-label="Delete" data-confirm-danger
//           data-confirm-match="acme">...
//
// The click-capture interceptor stops the original event, shows the modal,
// and only re-dispatches the original click when the user confirms. The
// `armed` dataset flag keeps the re-dispatched click from re-triggering us.

(function () {
    let dialog = null;
    let titleEl, bodyEl, confirmBtn, cancelBtn;
    let matchWrap, matchHint, matchToken, matchInput, matchCopy;
    let resolveFn = null;

    function mount() {
        if (dialog) return;
        dialog = document.createElement("dialog");
        dialog.id = "sm-confirm-modal";
        // Container styling (surface, border, shadow, backdrop) lives in
        // input.css under #sm-confirm-modal so it tracks --theme-* tokens.
        dialog.className = "w-full max-w-md p-0";
        dialog.setAttribute("aria-labelledby", "sm-confirm-title");
        dialog.innerHTML =
            '<div class="space-y-4 p-6">' +
                '<h2 id="sm-confirm-title" class="text-lg font-semibold"></h2>' +
                '<p class="text-sm text-muted"></p>' +
                '<div data-sm-confirm-match hidden class="space-y-2">' +
                    '<p data-sm-confirm-hint class="font-mono text-xs text-quiet"></p>' +
                    '<div class="flex items-center gap-2">' +
                        '<input type="text" readonly data-sm-confirm-token ' +
                               'class="field w-full text-sm">' +
                        '<button type="button" data-sm-confirm-copy ' +
                                'class="btn-ghost px-2 py-1 text-xs font-medium">copy</button>' +
                    '</div>' +
                    '<input type="text" autocomplete="off" spellcheck="false" ' +
                           'data-sm-confirm-typed class="field w-full text-sm">' +
                '</div>' +
                '<div class="flex items-center justify-end gap-2 pt-2">' +
                    '<button type="button" data-sm-confirm-cancel ' +
                            'class="btn-ghost px-3 py-1.5 text-sm font-medium"></button>' +
                    '<button type="button" data-sm-confirm-ok ' +
                            'class="px-3 py-1.5 text-sm font-medium"></button>' +
                '</div>' +
            '</div>';
        document.body.appendChild(dialog);
        titleEl    = dialog.querySelector("h2");
        bodyEl     = dialog.querySelector("p");
        confirmBtn = dialog.querySelector("[data-sm-confirm-ok]");
        cancelBtn  = dialog.querySelector("[data-sm-confirm-cancel]");
        matchWrap  = dialog.querySelector("[data-sm-confirm-match]");
        matchHint  = dialog.querySelector("[data-sm-confirm-hint]");
        matchToken = dialog.querySelector("[data-sm-confirm-token]");
        matchInput = dialog.querySelector("[data-sm-confirm-typed]");
        matchCopy  = dialog.querySelector("[data-sm-confirm-copy]");

        matchInput.addEventListener("input", () => {
            confirmBtn.disabled = matchInput.value.trim() !== matchToken.value;
        });
        matchInput.addEventListener("keydown", (e) => {
            if (e.key === "Enter" && !confirmBtn.disabled) {
                e.preventDefault();
                settle(true);
            }
        });
        matchCopy.addEventListener("click", async () => {
            const ok = await copyToken();
            matchCopy.textContent = ok ? "copied" : "copy failed";
            setTimeout(() => { matchCopy.textContent = "copy"; }, 1200);
            matchInput.focus();
        });

        confirmBtn.addEventListener("click", () => settle(true));
        cancelBtn.addEventListener("click",  () => settle(false));
        // Backdrop click cancels.
        dialog.addEventListener("click", (e) => {
            if (e.target === dialog) settle(false);
        });
        // ESC fires `cancel` (preventable; native default is to close).
        dialog.addEventListener("cancel", () => settle(false));
    }

    // A detached textarea is inert under a modal <dialog>, so the legacy
    // fallback has to copy the on-screen field (same trap share_modal.js hit).
    async function copyToken() {
        matchToken.focus();
        matchToken.select();
        try { matchToken.setSelectionRange(0, matchToken.value.length); } catch { /* noop */ }
        try {
            if (navigator.clipboard && navigator.clipboard.writeText) {
                await navigator.clipboard.writeText(matchToken.value);
                return true;
            }
        } catch { /* fall through to legacy */ }
        try { return document.execCommand("copy"); } catch { return false; }
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
        confirmBtn.textContent = opts.confirmLabel || "confirm";
        cancelBtn.textContent  = opts.cancelLabel  || "cancel";

        const isDanger = !!opts.danger;
        titleEl.className = "text-lg font-semibold " +
            (isDanger ? "text-state-bad" : "text-body");
        confirmBtn.className = "sticker-btn px-3 py-1.5 text-sm font-medium " +
            (isDanger ? "sticker-btn--danger" : "sticker-btn--primary");

        const match = opts.match ? String(opts.match) : "";
        matchWrap.hidden = !match;
        matchToken.value = match;
        matchInput.value = "";
        matchCopy.textContent = "copy";
        // Left enabled without a match, or a plain confirm would be unclickable.
        confirmBtn.disabled = !!match;
        if (match) {
            matchHint.textContent = "# type the name to confirm";
            matchToken.setAttribute("aria-label", "Name to type: " + match);
            matchInput.setAttribute("aria-label", "Type " + match + " to confirm");
        }

        // Re-entrance: a prior modal still open (e.g. caller didn't await).
        // Settle it false and close before re-opening, or showModal throws
        // InvalidStateError on an already-open dialog.
        if (dialog.open) {
            settle(false);
        }

        return new Promise((resolve) => {
            resolveFn = resolve;
            dialog.showModal();
            // Typing is the next step when a match is required; otherwise
            // pre-focus the safer button for destructive actions.
            if (match) matchInput.focus();
            else (isDanger ? cancelBtn : confirmBtn).focus();
        });
    };

    // smPrompt: trimmed string, array when splitOnComma, or null.
    let promptDialog = null;
    let promptTitleEl, promptBodyEl, promptInputEl, promptTextEl, promptActiveEl, promptOkBtn, promptCancelBtn;
    let promptResolve = null;

    function mountPrompt() {
        if (promptDialog) return;
        promptDialog = document.createElement("dialog");
        promptDialog.id = "sm-prompt-modal";
        promptDialog.className = "w-full max-w-md p-0";
        promptDialog.setAttribute("aria-labelledby", "sm-prompt-title");
        promptDialog.innerHTML =
            '<form method="dialog" class="space-y-4 p-6">' +
                '<h2 id="sm-prompt-title" class="text-lg font-semibold"></h2>' +
                '<p class="text-sm text-muted"></p>' +
                '<input type="text" class="field w-full text-sm" data-sm-prompt-input>' +
                '<textarea rows="5" class="field w-full text-sm" data-sm-prompt-text hidden></textarea>' +
                '<div class="flex items-center justify-end gap-2 pt-2">' +
                    '<button type="button" data-sm-prompt-cancel ' +
                            'class="btn-ghost px-3 py-1.5 text-sm font-medium">cancel</button>' +
                    '<button type="submit" data-sm-prompt-ok ' +
                            'class="sticker-btn sticker-btn--primary px-3 py-1.5 text-sm font-medium">ok</button>' +
                '</div>' +
            '</form>';
        document.body.appendChild(promptDialog);
        promptTitleEl  = promptDialog.querySelector("h2");
        promptBodyEl   = promptDialog.querySelector("p");
        promptInputEl  = promptDialog.querySelector("[data-sm-prompt-input]");
        promptTextEl   = promptDialog.querySelector("[data-sm-prompt-text]");
        promptActiveEl = promptInputEl;
        promptOkBtn    = promptDialog.querySelector("[data-sm-prompt-ok]");
        promptCancelBtn= promptDialog.querySelector("[data-sm-prompt-cancel]");

        promptDialog.querySelector("form").addEventListener("submit", (e) => {
            e.preventDefault();
            settlePrompt(promptActiveEl.value);
        });
        // Cmd/Ctrl+Enter submits the multi-line composer (Enter inserts a newline).
        promptTextEl.addEventListener("keydown", (e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
                e.preventDefault();
                settlePrompt(promptActiveEl.value);
            }
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
        const optional = promptDialog.dataset.optional === "1";
        if (split) {
            r(trimmed.split(",").map(s => s.trim()).filter(Boolean));
        } else if (trimmed === "") {
            // Optional prompts proceed with an empty string; required ones treat
            // empty the same as cancel. Cancel itself always resolves null above.
            r(optional ? "" : null);
        } else {
            r(trimmed);
        }
    }
    window.smPrompt = function (opts) {
        mountPrompt();
        opts = opts || {};
        const multiline = !!opts.multiline;
        promptActiveEl = multiline ? promptTextEl : promptInputEl;
        promptInputEl.hidden = multiline;
        promptTextEl.hidden = !multiline;
        promptTitleEl.textContent  = opts.title       || "Input";
        promptBodyEl.textContent   = opts.body        || "";
        promptActiveEl.placeholder = opts.placeholder || "";
        promptActiveEl.value       = opts.value       || "";
        promptDialog.dataset.split = opts.splitOnComma ? "1" : "0";
        promptDialog.dataset.optional = opts.optional ? "1" : "0";
        if (promptDialog.open) settlePrompt(null);
        return new Promise((resolve) => {
            promptResolve = resolve;
            promptDialog.showModal();
            promptActiveEl.focus();
            if (!multiline) promptActiveEl.select();
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
            confirmLabel: d.confirmLabel,
            cancelLabel:  d.confirmCancel,
            danger:       d.confirmDanger !== undefined,
            match:        d.confirmMatch,
        }).then(function (ok) {
            if (!ok) return;
            trigger.dataset.confirmModalArmed = "1";
            try { trigger.click(); } finally {
                queueMicrotask(() => { delete trigger.dataset.confirmModalArmed; });
            }
        });
    }, true);
})();
