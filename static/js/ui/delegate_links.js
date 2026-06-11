// Delegation links on the channels settings page: mint, list, revoke.
(function () {
    "use strict";

    const box = document.querySelector("[data-delegate-box]");
    if (!box) return;
    const mintBtn = box.querySelector("[data-delegate-mint]");
    const result = box.querySelector("[data-delegate-result]");
    const urlBox = box.querySelector("[data-delegate-url-box]");
    const urlInput = box.querySelector("[data-delegate-url]");
    const copyBtn = box.querySelector("[data-delegate-copy]");
    const list = box.querySelector("[data-delegate-list]");
    const headers = { "Accept": "application/json", "X-Requested-With": "uptimepage" };

    function show(text, cls) {
        result.textContent = text;
        result.className = `font-mono text-xs ${cls}`;
    }

    async function refreshList() {
        let rows;
        try {
            const res = await fetch("/api/v1/notification-channels/delegate", { headers });
            if (!res.ok) return;
            rows = await res.json();
        } catch {
            return;
        }
        const pending = rows.filter((r) => r.status === "pending");
        list.innerHTML = "";
        if (!pending.length) return;
        for (const r of pending) {
            const line = document.createElement("p");
            line.className = "font-mono text-xs text-quiet flex items-center gap-2";
            const label = document.createElement("span");
            const until = new Date(r.expires_at).toLocaleDateString();
            label.textContent = `# pending invite${r.name ? ` "${r.name}"` : ""}${r.kind ? ` (${r.kind})` : ""} — expires ${until}`;
            const revoke = document.createElement("button");
            revoke.type = "button";
            revoke.textContent = "revoke";
            revoke.className = "underline decoration-dotted hover:text-body";
            revoke.addEventListener("click", async () => {
                revoke.disabled = true;
                try {
                    await fetch(`/api/v1/notification-channels/delegate/${r.id}`, { method: "DELETE", headers });
                } finally {
                    refreshList();
                }
            });
            line.append(label, revoke);
            list.append(line);
        }
    }

    mintBtn.addEventListener("click", async () => {
        mintBtn.disabled = true;
        show("# creating a single-use link…", "text-quiet");
        try {
            const res = await fetch("/api/v1/notification-channels/delegate", {
                method: "POST",
                headers: { ...headers, "Content-Type": "application/json" },
                body: JSON.stringify({}),
            });
            if (!res.ok) {
                const msg = await window.smApiErrorMessage(res, `mint failed (${res.status})`);
                show(`✗ ${msg}`, "flash-text flash-text--bad font-medium");
                return;
            }
            const body = await res.json();
            urlInput.value = body.url;
            urlBox.classList.remove("hidden");
            show("✓ link created — send it to the channel owner; valid 7 days, single use", "flash-text flash-text--ok font-medium");
            refreshList();
        } catch (err) {
            show(`✗ network error: ${err.message || err}`, "flash-text flash-text--bad font-medium");
        } finally {
            mintBtn.disabled = false;
        }
    });

    copyBtn.addEventListener("click", async () => {
        try {
            await navigator.clipboard.writeText(urlInput.value);
            copyBtn.textContent = "copied";
            setTimeout(() => { copyBtn.textContent = "copy"; }, 1500);
        } catch {
            urlInput.select();
        }
    });

    refreshList();
})();
