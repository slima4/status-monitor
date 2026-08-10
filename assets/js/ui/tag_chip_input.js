// Tag selector. Existing org tags are scope-token checkboxes; the + button
// reveals an input that adds a brand-new tag as a checked chip. Selection is
// the set of checked chips; smCollectTags() returns it for check_form.js.

(function () {
    const container = document.querySelector("[data-tag-chips]");
    if (!container) return;
    const addBtn = container.querySelector("[data-tag-add]");
    const newInput = container.querySelector("[data-tag-new]");
    if (!addBtn || !newInput) return;

    function picks() {
        return Array.from(container.querySelectorAll("[data-tag-pick]"));
    }

    const maxTags = Number(newInput.dataset.tagMax) || 0;

    function checkedCount() {
        return picks().filter(c => c.checked).length;
    }

    function atCap() {
        return maxTags > 0 && checkedCount() >= maxTags;
    }

    function refuse(message) {
        newInput.classList.remove("hidden");
        newInput.setCustomValidity(message);
        newInput.reportValidity();
    }

    // False keeps the input open, so a refusal is never mistaken for a tag that
    // was added and is now off-screen.
    function addTag(raw) {
        const value = (raw || "").trim();
        if (!value) return true;
        if (atCap()) {
            refuse(`A monitor takes at most ${maxTags} tags.`);
            return false;
        }
        const lower = value.toLowerCase();
        // Adopt an existing tag's casing rather than fork `prod` next to `Prod`.
        const existing = picks().find(c => c.value.toLowerCase() === lower);
        if (existing) {
            existing.checked = true;
        } else {
            const label = document.createElement("label");
            label.className = "scope-token";
            const cb = document.createElement("input");
            cb.type = "checkbox";
            cb.setAttribute("data-tag-pick", "");
            cb.value = value;
            cb.className = "sr-only";
            cb.checked = true;
            label.appendChild(cb);
            label.appendChild(document.createTextNode(" " + value));
            container.insertBefore(label, newInput);
        }
        container.dispatchEvent(new Event("change", { bubbles: true }));
        return true;
    }

    function closeInput() {
        newInput.value = "";
        newInput.classList.add("hidden");
    }

    addBtn.addEventListener("click", () => {
        newInput.classList.remove("hidden");
        newInput.focus();
    });
    newInput.addEventListener("input", () => newInput.setCustomValidity(""));
    newInput.addEventListener("keydown", (evt) => {
        if (evt.key === "Enter" || evt.key === ",") {
            evt.preventDefault();
            if (addTag(newInput.value)) newInput.value = "";
        } else if (evt.key === "Escape") {
            closeInput();
        }
    });
    newInput.addEventListener("blur", () => {
        if (addTag(newInput.value)) closeInput();
    });

    // Checking an existing chip is the easier way past the cap than typing one.
    container.addEventListener("change", (evt) => {
        const chip = evt.target.closest("[data-tag-pick]");
        if (!chip || !chip.checked || !maxTags) return;
        if (checkedCount() > maxTags) {
            chip.checked = false;
            refuse(`A monitor takes at most ${maxTags} tags.`);
        }
    });

    window.smCollectTags = () => picks().filter(c => c.checked).map(c => c.value);
})();
