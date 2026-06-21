// On-call schedule builder. Manages the dynamic layer rows and serialises them
// into a NewOnCallSchedule body for POST/PATCH /api/v1/on-call/schedules.
// Shares the error-banner layer from api_form.js (loaded before this).
(function () {
    const form = document.getElementById("schedule-form");
    if (!form) return;
    const layers = document.getElementById("layers");
    const tmpl = document.getElementById("layer-template");
    const addBtn = document.getElementById("add-layer");

    const UNIT_SECS = { daily: 86400, weekly: 604800, custom: 1 };
    const UNIT_LABEL = { daily: "day(s)", weekly: "week(s)", custom: "second(s)" };

    function pad(n) { return String(n).padStart(2, "0"); }

    // RFC3339 instant → the value a <input type=datetime-local> expects, in the
    // browser's local zone.
    function isoToLocalInput(iso) {
        if (!iso) return "";
        const d = new Date(iso);
        if (isNaN(d.getTime())) return "";
        return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
    }

    function syncUnit(row) {
        const type = row.querySelector("[data-rotation-type]").value;
        const unit = row.querySelector("[data-rotation-unit]");
        if (unit) unit.textContent = UNIT_LABEL[type] || "";
    }

    function initRow(row) {
        syncUnit(row);
        row.querySelector("[data-rotation-type]").addEventListener("change", () => syncUnit(row));
        const handoff = row.querySelector("[data-handoff]");
        if (handoff && !handoff.value) {
            handoff.value = isoToLocalInput(handoff.getAttribute("data-iso"));
        }
    }

    function renumber() {
        layers.querySelectorAll("[data-layer-row]").forEach((row, i) => {
            const n = row.querySelector("[data-layer-num]");
            if (n) n.textContent = String(i + 1);
        });
    }

    layers.querySelectorAll("[data-layer-row]").forEach(initRow);

    addBtn.addEventListener("click", () => {
        const frag = tmpl.content.cloneNode(true);
        layers.appendChild(frag);
        initRow(layers.lastElementChild);
        renumber();
    });

    layers.addEventListener("click", (evt) => {
        const rm = evt.target.closest("[data-remove-layer]");
        if (!rm) return;
        if (layers.querySelectorAll("[data-layer-row]").length <= 1) {
            renderClientError("A schedule needs at least one layer.");
            return;
        }
        rm.closest("[data-layer-row]").remove();
        renumber();
    });

    const submitBtn = form.querySelector("button[type=submit]");
    form.addEventListener("submit", async (evt) => {
        evt.preventDefault();
        if (submitBtn.disabled) return;
        clearErrors();
        const built = buildBody();
        if (built.error) { renderClientError(built.error); return; }
        const label = submitBtn.textContent;
        submitBtn.disabled = true;
        submitBtn.textContent = "Saving…";
        let navigating = false;
        try {
            let res;
            try {
                res = await fetch(form.dataset.action, {
                    method: form.dataset.method,
                    headers: {
                        "Content-Type": "application/json",
                        "Accept": "application/json",
                        "X-Requested-With": "uptimepage",
                    },
                    body: JSON.stringify(built.payload),
                });
            } catch (err) {
                renderClientError(`Network error: ${err.message || err}`);
                return;
            }
            if (res.ok) { navigating = true; window.location = "/settings/on-call"; return; }
            let body;
            try { body = await res.json(); }
            catch { renderClientError(`Request failed (${res.status})`); return; }
            renderApiError(body, res.status);
        } finally {
            if (!navigating) { submitBtn.disabled = false; submitBtn.textContent = label; }
        }
    });

    function buildBody() {
        const name = (form.querySelector("[name=name]").value || "").trim();
        if (!name) return { error: "Name is required." };
        const timezone = (form.querySelector("[name=timezone]").value || "").trim() || "UTC";
        const rows = Array.from(layers.querySelectorAll("[data-layer-row]"));
        const out = [];
        for (let i = 0; i < rows.length; i++) {
            const row = rows[i];
            const type = row.querySelector("[data-rotation-type]").value;
            const length = parseInt(row.querySelector("[data-rotation-length]").value, 10);
            if (!Number.isFinite(length) || length < 1) {
                return { error: `Layer ${i + 1}: rotation length must be at least 1.` };
            }
            const handoffVal = row.querySelector("[data-handoff]").value;
            if (!handoffVal) return { error: `Layer ${i + 1}: pick a first handoff time.` };
            const handoff = new Date(handoffVal);
            if (isNaN(handoff.getTime())) return { error: `Layer ${i + 1}: invalid handoff time.` };
            const participants = Array.from(row.querySelectorAll("[data-participant]:checked"))
                .map((c) => ({ user_id: c.value }));
            if (participants.length === 0) {
                return { error: `Layer ${i + 1} needs at least one participant.` };
            }
            const layerName = (row.querySelector("[data-layer-name]").value || "").trim();
            out.push({
                name: layerName || null,
                rotation_type: type,
                rotation_length_secs: length * (UNIT_SECS[type] || 1),
                handoff_at: handoff.toISOString(),
                layer_order: i,
                participants,
            });
        }
        return { payload: { name, timezone, layers: out } };
    }

    function clearErrors() { window.smClearFormErrors(document.getElementById("form-errors")); }
    function renderClientError(msg) { window.smRenderClientError(document.getElementById("form-errors"), msg); }
    function renderApiError(json, status) { window.smRenderApiError(document.getElementById("form-errors"), json, status); }
})();
