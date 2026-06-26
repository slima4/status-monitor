// Key/value row repeater for the HTTP headers section of the monitor form.
// Hydrates from server-rendered <div data-header-row> children, lets the
// user add/remove rows, and exposes a `collect()` helper that check_form.js
// calls on submit to build the headers JSON object.

(function () {
    const container = document.querySelector("[data-header-rows]");
    const addBtn = document.querySelector("[data-header-add]");
    if (!container || !addBtn) return;

    function rowHtml() {
        return `
            <input type="text" name="http_header_key"
                   placeholder="Header name"
                   class="flex-1 min-w-[10rem] field font-mono">
            <input type="text" name="http_header_value"
                   placeholder="Value"
                   class="flex-[2] min-w-[14rem] field font-mono">
            <button type="button" data-header-remove
                    class="btn-ghost px-2 text-sm" aria-label="Remove header">×</button>
        `;
    }

    function addRow(focus) {
        const row = document.createElement("div");
        row.dataset.headerRow = "";
        row.className = "flex flex-wrap items-center gap-2";
        row.innerHTML = rowHtml();
        container.appendChild(row);
        if (focus) row.querySelector("input").focus();
        return row;
    }

    addBtn.addEventListener("click", () => addRow(true));

    // Public adder: the variable auth picker calls this to drop a prefilled
    // header row (e.g. `Authorization: Bearer {{key}}`) without duplicating the
    // row markup.
    window.smAddHeaderRow = function (name, value) {
        const rows = Array.from(container.querySelectorAll("[data-header-row]"));
        const keyEl = (r) => r.querySelector("[name='http_header_key']");
        const valEl = (r) => r.querySelector("[name='http_header_value']");
        // Reuse a row already targeting this header so the picker never adds a
        // duplicate that smCollectHeaders would silently collapse to the last.
        let row = rows.find((r) => keyEl(r).value.trim().toLowerCase() === name.toLowerCase());
        if (!row) row = rows.find((r) => keyEl(r).value.trim() === "" && valEl(r).value.trim() === "");
        if (!row) row = addRow(false);
        keyEl(row).value = name;
        valEl(row).value = value;
        return row;
    };

    container.addEventListener("click", (evt) => {
        const btn = evt.target.closest("[data-header-remove]");
        if (!btn) return;
        const row = btn.closest("[data-header-row]");
        if (row) row.remove();
    });

    // Public collector — `check_form.js` reads this on submit + test-now.
    window.smCollectHeaders = function () {
        const out = {};
        for (const row of container.querySelectorAll("[data-header-row]")) {
            const key = row.querySelector("[name='http_header_key']").value.trim();
            const value = row.querySelector("[name='http_header_value']").value;
            if (key.length === 0) continue;
            out[key] = value;
        }
        return out;
    };
})();
