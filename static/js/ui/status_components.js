// Status-page component curation: autosave + reorder. The rows are an
// htmx partial swapped into #status-components, so every binding is
// delegated on that stable container (no rebinding after each swap).
// Each change is a PATCH /api/v1/targets/{id} (same JSON API + session
// auth the rest of the app uses); reorder persists public_sort_order
// only for rows whose position actually changed.
(function () {
    const root = document.getElementById("status-components");
    if (!root) return;

    const SAVE_DEBOUNCE_MS = 600;
    const ORDER_DEBOUNCE_MS = 400;
    const timers = new Map(); // `${id}:${field}` -> timeout
    let orderTimer = null;

    // htmx replaces #status-components' innerHTML on refresh; cancel any
    // pending debounced save so a timer can't fire against a detached row
    // (the fresh partial carries authoritative values anyway).
    root.addEventListener("htmx:beforeSwap", () => {
        timers.forEach(clearTimeout);
        timers.clear();
        clearTimeout(orderTimer);
        orderTimer = null;
    });

    function setStatus(msg, ok) {
        const el = document.getElementById("components-status");
        if (!el) return;
        el.textContent = msg;
        el.className = "px-4 py-2 text-xs " + (ok ? "text-emerald-600" : "text-rose-600");
    }

    async function patch(id, body) {
        try {
            const res = await fetch(`/api/v1/targets/${id}`, {
                method: "PATCH",
                headers: {
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "X-Requested-With": "status-monitor",
                },
                credentials: "same-origin",
                body: JSON.stringify(body),
            });
            if (!res.ok) {
                let msg = `Save failed (${res.status})`;
                try {
                    const b = await res.json();
                    if (b && b.error && b.error.message) msg = b.error.message;
                } catch { /* non-JSON body: keep the status-code message */ }
                setStatus(msg, false);
                return false;
            }
            setStatus("Saved.", true);
            return true;
        } catch (err) {
            setStatus(`Network error: ${err.message || err}`, false);
            return false;
        }
    }

    function fieldBody(field, input) {
        if (field === "public_status") return { public_status: input.checked };
        const v = input.value.trim();
        return { [field]: v.length ? v : null };
    }

    function saveField(row, input) {
        const id = row.dataset.id;
        const field = input.dataset.field;
        patch(id, fieldBody(field, input));
    }

    // Checkbox: save immediately. Text: debounce so typing isn't one
    // request per keystroke.
    root.addEventListener("change", (e) => {
        const input = e.target.closest("[data-field]");
        const row = e.target.closest("[data-component-row]");
        if (!input || !row) return;
        if (input.dataset.field === "public_status") saveField(row, input);
    });

    root.addEventListener("input", (e) => {
        const input = e.target.closest("[data-field]");
        const row = e.target.closest("[data-component-row]");
        if (!input || !row || input.dataset.field === "public_status") return;
        const key = `${row.dataset.id}:${input.dataset.field}`;
        clearTimeout(timers.get(key));
        timers.set(key, setTimeout(() => {
            timers.delete(key);
            saveField(row, input);
        }, SAVE_DEBOUNCE_MS));
    });

    // Persist order only where it changed: the row's index becomes its
    // public_sort_order; data-order tracks the last saved value so an
    // unmoved row isn't re-PATCHed. Writes are sequential (one drag shifts
    // every row below it — awaiting avoids a burst of parallel, possibly
    // out-of-order PATCHes against the shared per-org write budget).
    async function persistOrder() {
        const list = document.getElementById("components-rows");
        if (!list) return;
        const rows = [...list.querySelectorAll("[data-component-row]")];
        for (let i = 0; i < rows.length; i++) {
            const row = rows[i];
            if (Number(row.dataset.order) === i) continue;
            row.dataset.order = i;
            await patch(row.dataset.id, { public_sort_order: i });
        }
    }

    // Coalesce rapid successive moves into one persistence pass.
    function scheduleOrderSave() {
        clearTimeout(orderTimer);
        orderTimer = setTimeout(persistOrder, ORDER_DEBOUNCE_MS);
    }

    // --- Drag-and-drop reorder ---
    let dragged = null;

    root.addEventListener("dragstart", (e) => {
        const row = e.target.closest("[data-component-row]");
        if (!row) return;
        dragged = row;
        e.dataTransfer.effectAllowed = "move";
        row.classList.add("opacity-50");
    });

    root.addEventListener("dragend", () => {
        if (dragged) dragged.classList.remove("opacity-50");
        dragged = null;
    });

    root.addEventListener("dragover", (e) => {
        if (!dragged) return;
        e.preventDefault();
        const over = e.target.closest("[data-component-row]");
        if (!over || over === dragged) return;
        const rect = over.getBoundingClientRect();
        const after = e.clientY > rect.top + rect.height / 2;
        over.parentNode.insertBefore(dragged, after ? over.nextSibling : over);
    });

    root.addEventListener("drop", (e) => {
        if (!dragged) return;
        e.preventDefault();
        scheduleOrderSave();
    });

    // Keyboard parity for the drag handle: ↑/↓ move the row and save.
    root.addEventListener("keydown", (e) => {
        if (!e.target.matches("[data-drag-handle]")) return;
        if (e.key !== "ArrowUp" && e.key !== "ArrowDown") return;
        const row = e.target.closest("[data-component-row]");
        if (!row) return;
        e.preventDefault();
        const sibling = e.key === "ArrowUp"
            ? row.previousElementSibling
            : row.nextElementSibling;
        if (!sibling || !sibling.matches("[data-component-row]")) return;
        if (e.key === "ArrowUp") row.parentNode.insertBefore(row, sibling);
        else row.parentNode.insertBefore(sibling, row);
        e.target.focus();
        scheduleOrderSave();
    });
})();
