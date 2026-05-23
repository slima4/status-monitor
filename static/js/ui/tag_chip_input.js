// Tag chip input with autocomplete. Hydrates from server-rendered
// <span class="tag-chip" data-tag-value="..."> children, suggests existing
// tags from GET /api/v1/tags?q=&limit=10, and exposes `smCollectTags()`
// for check_form.js to read on submit.

(function () {
    const container = document.querySelector("[data-tag-chips]");
    if (!container) return;
    const input = container.querySelector("[data-tag-input]");
    const suggBox = container.querySelector("[data-tag-suggestions]");
    if (!input || !suggBox) return;

    const DEBOUNCE_MS = 200;
    let debounceTimer = null;
    let activeIndex = -1;
    // Per-page-load cache: typing "prod" across multiple monitor edits in
    // one session hits one PG query instead of N.
    const suggestionCache = new Map();

    function currentValues() {
        return Array.from(container.querySelectorAll("[data-tag-value]"))
            .map(el => el.dataset.tagValue);
    }

    function commit(raw) {
        const value = (raw || "").trim();
        if (!value) return;
        // Tags are case-insensitive lookups; dedupe by lowercased value but
        // keep the user's casing in the rendered chip.
        const existing = currentValues().map(v => v.toLowerCase());
        if (existing.includes(value.toLowerCase())) {
            input.value = "";
            return;
        }
        const chip = document.createElement("span");
        chip.className = "tag-chip";
        chip.dataset.tagValue = value;
        chip.innerHTML = `
            <span class="tag-chip__label"></span>
            <button type="button" class="tag-chip__remove" aria-label="Remove tag">×</button>
        `;
        chip.querySelector(".tag-chip__label").textContent = value;
        container.insertBefore(chip, input);
        input.value = "";
        hideSuggestions();
    }

    function removeLast() {
        const chips = container.querySelectorAll(".tag-chip");
        if (chips.length === 0) return;
        chips[chips.length - 1].remove();
    }

    container.addEventListener("click", (evt) => {
        const btn = evt.target.closest(".tag-chip__remove");
        if (!btn) return;
        btn.closest(".tag-chip").remove();
        input.focus();
    });

    input.addEventListener("keydown", (evt) => {
        if (evt.key === "Enter" || evt.key === ",") {
            evt.preventDefault();
            if (activeIndex >= 0) {
                const items = suggBox.querySelectorAll("li");
                if (items[activeIndex]) {
                    commit(items[activeIndex].dataset.value);
                    return;
                }
            }
            commit(input.value);
        } else if (evt.key === "Backspace" && input.value.length === 0) {
            removeLast();
        } else if (evt.key === "ArrowDown") {
            evt.preventDefault();
            moveActive(1);
        } else if (evt.key === "ArrowUp") {
            evt.preventDefault();
            moveActive(-1);
        } else if (evt.key === "Escape") {
            hideSuggestions();
        }
    });

    input.addEventListener("blur", (evt) => {
        // If focus moved into the suggestion box, its mousedown handler
        // will commit; don't pre-empt it. Otherwise commit the typed
        // value as a literal chip and close the dropdown.
        if (evt.relatedTarget && suggBox.contains(evt.relatedTarget)) return;
        commit(input.value);
        hideSuggestions();
    });

    input.addEventListener("input", () => {
        clearTimeout(debounceTimer);
        const q = input.value.trim();
        if (q.length === 0) {
            hideSuggestions();
            return;
        }
        debounceTimer = setTimeout(() => fetchSuggestions(q), DEBOUNCE_MS);
    });

    async function fetchSuggestions(q) {
        const key = q.toLowerCase();
        let items = suggestionCache.get(key);
        if (!items) {
            let json;
            try {
                const r = await fetch(`/api/v1/tags?q=${encodeURIComponent(q)}&limit=10`, {
                    headers: { "Accept": "application/json", "X-Requested-With": "status-monitor" },
                });
                if (!r.ok) { hideSuggestions(); return; }
                json = await r.json();
            } catch {
                hideSuggestions();
                return;
            }
            items = json.items || [];
            suggestionCache.set(key, items);
        }
        const taken = new Set(currentValues().map(v => v.toLowerCase()));
        const filtered = items
            .filter(t => !taken.has(String(t.name).toLowerCase()))
            .slice(0, 10);
        if (filtered.length === 0) { hideSuggestions(); return; }
        renderSuggestions(filtered);
    }

    function renderSuggestions(items) {
        suggBox.innerHTML = "";
        items.forEach((item, idx) => {
            const li = document.createElement("li");
            li.setAttribute("role", "option");
            li.dataset.value = item.name;
            li.textContent = `${item.name} (${item.count})`;
            li.addEventListener("mousedown", (evt) => {
                evt.preventDefault();
                commit(item.name);
                input.focus();
            });
            li.addEventListener("mouseenter", () => setActive(idx));
            suggBox.appendChild(li);
        });
        activeIndex = -1;
        suggBox.classList.remove("hidden");
    }

    function moveActive(delta) {
        const items = suggBox.querySelectorAll("li");
        if (items.length === 0) return;
        if (suggBox.classList.contains("hidden")) suggBox.classList.remove("hidden");
        let next = activeIndex + delta;
        if (next < 0) next = items.length - 1;
        if (next >= items.length) next = 0;
        setActive(next);
    }

    function setActive(idx) {
        const items = suggBox.querySelectorAll("li");
        items.forEach(li => li.setAttribute("aria-selected", "false"));
        if (items[idx]) {
            items[idx].setAttribute("aria-selected", "true");
            items[idx].scrollIntoView({ block: "nearest" });
        }
        activeIndex = idx;
    }

    function hideSuggestions() {
        suggBox.classList.add("hidden");
        suggBox.innerHTML = "";
        activeIndex = -1;
    }

    window.smCollectTags = currentValues;
})();
