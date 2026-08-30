// Share-link manager for the monitor detail header. A single reused <dialog>
// lists a monitor's active read-only links — each with its full URL visible in a
// read-only field — mints new ones, copies any link, and revokes them. Writes
// go through /api/v1/targets/{id}/shares with the CSRF header. Tokens are
// re-copyable (stored encrypted at rest server-side).

(function () {
    const CSRF = { "X-Requested-With": "uptimepage" };
    let dialog = null;
    let titleEl, listEl, emptyEl, createErr, labelInput, expiresInput;
    let targetId = null;

    function esc(s) {
        return window.smEscapeHtml ? window.smEscapeHtml(s) : String(s);
    }

    function fmt(iso) {
        if (!iso) return "never";
        const d = new Date(iso);
        return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
    }

    function shareUrl(token) {
        return window.location.origin + "/m/" + token;
    }

    // Copy from a visible input INSIDE the dialog. A temp element on document.body
    // is inert under a modal <dialog>, so execCommand there copies nothing while
    // still reporting success — copying the on-screen field avoids that trap.
    async function copyInput(input) {
        input.focus();
        input.select();
        try { input.setSelectionRange(0, input.value.length); } catch { /* noop */ }
        try {
            if (navigator.clipboard && navigator.clipboard.writeText) {
                await navigator.clipboard.writeText(input.value);
                return true;
            }
        } catch { /* fall through to legacy */ }
        try { return document.execCommand("copy"); } catch { return false; }
    }

    function mount() {
        if (dialog) return;
        dialog = document.createElement("dialog");
        dialog.id = "sm-share-modal";
        dialog.className = "w-full max-w-lg p-0";
        dialog.setAttribute("aria-labelledby", "sm-share-title");
        dialog.innerHTML =
            '<div class="space-y-4 p-6">' +
              '<div class="flex items-center justify-between gap-3">' +
                '<h2 id="sm-share-title" class="text-lg font-semibold"></h2>' +
                '<button type="button" data-share-close aria-label="Close" ' +
                        'class="btn-ghost px-2 py-1 text-sm">✕</button>' +
              '</div>' +
              '<p class="text-sm text-muted">Anyone with a link can view this monitor read-only — ' +
                'no account needed. Credentials in the check config are hidden.</p>' +
              '<form data-share-create class="space-y-2">' +
                '<div class="flex flex-wrap items-end gap-2">' +
                  '<label class="flex-1 min-w-48 text-xs text-muted">Label (optional)' +
                    '<input name="label" type="text" maxlength="80" autocomplete="off" ' +
                           'placeholder="e.g. Slack #ops" class="field mt-1 w-full"></label>' +
                  '<label class="text-xs text-muted">Expires (optional)' +
                    '<input name="expires_at" type="datetime-local" class="field mt-1"></label>' +
                  '<button type="submit" class="sticker-btn sticker-btn--accent px-3 py-1.5 text-sm font-medium">Create link</button>' +
                '</div>' +
                '<div data-share-create-error class="hidden alert-card alert-card--error" aria-live="polite"></div>' +
              '</form>' +
              '<div>' +
                '<h3 class="panel-label">Active links</h3>' +
                '<ul data-share-list class="mt-2"></ul>' +
                '<p data-share-empty class="hidden py-2 text-sm text-muted">No active links.</p>' +
              '</div>' +
            '</div>';
        document.body.appendChild(dialog);

        titleEl    = dialog.querySelector("#sm-share-title");
        listEl     = dialog.querySelector("[data-share-list]");
        emptyEl    = dialog.querySelector("[data-share-empty]");
        createErr  = dialog.querySelector("[data-share-create-error]");
        labelInput = dialog.querySelector('[data-share-create] [name="label"]');
        expiresInput = dialog.querySelector('[data-share-create] [name="expires_at"]');

        dialog.querySelector("[data-share-close]").addEventListener("click", () => dialog.close());
        dialog.addEventListener("click", (e) => { if (e.target === dialog) dialog.close(); });
        dialog.querySelector("[data-share-create]").addEventListener("submit", onCreate);
        listEl.addEventListener("click", onListClick);
    }

    async function loadList() {
        listEl.innerHTML = "";
        emptyEl.classList.add("hidden");
        try {
            // no-store: /api/v1 reads carry max-age=10, so the browser would
            // otherwise serve a stale list right after a create/revoke.
            const r = await fetch(`/api/v1/targets/${encodeURIComponent(targetId)}/shares`, {
                headers: { "Accept": "application/json" },
                cache: "no-store",
            });
            if (!r.ok) throw new Error(`HTTP ${r.status}`);
            const shares = await r.json();
            if (!Array.isArray(shares) || shares.length === 0) {
                emptyEl.classList.remove("hidden");
                return;
            }
            listEl.innerHTML = shares.map(rowHtml).join("");
        } catch (err) {
            emptyEl.textContent = `Could not load links: ${String(err.message || err)}`;
            emptyEl.classList.remove("hidden");
        }
    }

    function rowHtml(s) {
        const pages = Array.isArray(s.used_by_pages) ? s.used_by_pages : [];
        // Page-minted links carry no label; name them by their page.
        const label = s.label
            ? esc(s.label)
            : pages.length
              ? esc(pages.map((p) => p.name).join(", "))
              : '<span class="text-muted">(no label)</span>';
        const usedBy = pages.length
            ? `<div class="text-xs text-muted">detail link on ${pages
                  .map((p) => `<span class="font-mono">${esc(p.slug)}</span>`)
                  .join(", ")} — revoking removes it there</div>`
            : "";
        const views = Number(s.view_count || 0);
        const viewsLabel = views === 1 ? "1 view" : `${views} views`;
        const lastSeen = s.last_viewed_at ? `last opened ${esc(fmt(s.last_viewed_at))}` : "never opened";
        const linkRow = s.token
            ? `<div class="flex gap-2">
                 <input data-share-url readonly value="${esc(shareUrl(s.token))}" aria-label="Share link"
                        class="field w-full font-mono text-xs">
                 <button type="button" data-share-copy class="shrink-0 btn-ghost px-2.5 py-1 text-xs">copy</button>
               </div>`
            : '<p class="text-xs text-muted" title="No encryption key configured to recover this link">Link unavailable — encryption key not configured.</p>';
        return `<li class="space-y-2 border-t border-[color:var(--theme-line)] py-3 text-sm">
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <div class="truncate">${label}</div>
              <div class="text-xs text-muted">created ${esc(fmt(s.created_at))} · expires ${esc(fmt(s.expires_at))}</div>
              <div class="text-xs text-muted">${esc(viewsLabel)} · ${esc(lastSeen)}</div>
              ${usedBy}
            </div>
            <button type="button" data-share-revoke data-share-id="${esc(s.id)}"
                    class="shrink-0 btn-ghost btn-ghost--danger px-2.5 py-1 text-xs focus-visible:ring-red-500">revoke</button>
          </div>
          ${linkRow}
        </li>`;
    }

    async function onCreate(e) {
        e.preventDefault();
        createErr.classList.add("hidden");
        const body = {};
        const label = labelInput.value.trim();
        if (label) body.label = label;
        if (expiresInput.value) {
            const d = new Date(expiresInput.value);
            if (Number.isNaN(d.getTime())) {
                showCreateError("Invalid expiry date.");
                return;
            }
            body.expires_at = d.toISOString();
        }
        try {
            const r = await fetch(`/api/v1/targets/${encodeURIComponent(targetId)}/shares`, {
                method: "POST",
                headers: { "Content-Type": "application/json", "Accept": "application/json", ...CSRF },
                body: JSON.stringify(body),
            });
            const json = await r.json().catch(() => null);
            if (!r.ok) {
                if (window.smRenderApiError) window.smRenderApiError(createErr, json, r.status);
                else showCreateError("Could not create link.");
                createErr.classList.remove("hidden");
                return;
            }
            labelInput.value = "";
            expiresInput.value = "";
            await loadList();
            // Select the new link's field (top of the list) so it's ready to copy.
            const first = listEl.querySelector("[data-share-url]");
            if (first) first.select();
            if (window.smToast) window.smToast({ message: "Link created", kind: "ok" });
        } catch (err) {
            showCreateError(`network: ${String(err.message || err)}`);
        }
    }

    function showCreateError(msg) {
        createErr.textContent = msg;
        createErr.classList.remove("hidden");
    }

    async function onListClick(e) {
        const copyBtn = e.target.closest("[data-share-copy]");
        if (copyBtn) {
            const input = copyBtn.parentElement.querySelector("[data-share-url]");
            const ok = input ? await copyInput(input) : false;
            if (window.smToast) {
                window.smToast(ok
                    ? { message: "Link copied", kind: "ok" }
                    : { message: "Select the link and press ⌘/Ctrl+C", kind: "info" });
            }
            return;
        }
        const revokeBtn = e.target.closest("[data-share-revoke]");
        if (!revokeBtn) return;
        const shareId = revokeBtn.dataset.shareId;
        const ok = window.smConfirm
            ? await window.smConfirm({
                  title: "Revoke link?",
                  body: "Anyone holding this link loses access immediately. This cannot be undone.",
                  confirmLabel: "Revoke",
                  danger: true,
              })
            : true;
        if (!ok) return;
        try {
            const r = await fetch(
                `/api/v1/targets/${encodeURIComponent(targetId)}/shares/${encodeURIComponent(shareId)}`,
                { method: "DELETE", headers: { "Accept": "application/json", ...CSRF } },
            );
            if (!r.ok && r.status !== 404) throw new Error(`HTTP ${r.status}`);
            if (window.smToast) window.smToast({ message: "Link revoked", kind: "info" });
            loadList();
        } catch (err) {
            if (window.smToast) window.smToast({ message: `Revoke failed: ${String(err.message || err)}` });
        }
    }

    function open(btn) {
        mount();
        targetId = btn.dataset.targetId;
        titleEl.textContent = `Share ${btn.dataset.targetName || "monitor"}`;
        createErr.classList.add("hidden");
        labelInput.value = "";
        expiresInput.value = "";
        loadList();
        dialog.showModal();
    }

    document.addEventListener("click", (e) => {
        const btn = e.target.closest("[data-share-open]");
        if (btn) {
            e.preventDefault();
            open(btn);
        }
    });
})();
