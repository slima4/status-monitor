// Flow step builder: add/remove/drag-reorder step rows and toggle each row's
// fields to the selected op. Exposes window.smCollectFlowSteps() for
// check_form.js to serialise into the flow check body.
(function () {
    const form = document.getElementById("check-form");
    if (!form) return;
    const wrap = form.querySelector("[data-flow-steps]");
    if (!wrap) return;
    const tmpl = document.getElementById("flow-step-template");
    const addBtn = form.querySelector("[data-flow-add]");

    // Which fields each op shows; everything else in the row stays hidden.
    const OP_FIELDS = {
        goto: ["url"],
        click: ["selector"],
        fill: ["selector", "value"],
        wait_for: ["selector"],
        assert_text: ["selector", "contains"],
        assert_url: ["contains"],
    };

    const rows = () => Array.from(wrap.querySelectorAll("[data-flow-row]"));

    function renumber() {
        rows().forEach((row, i) => {
            const n = row.querySelector("[data-flow-num]");
            if (n) n.textContent = i + 1;
        });
    }

    function applyOp(row) {
        const op = row.querySelector("[data-flow-op]").value;
        const show = OP_FIELDS[op] || [];
        row.querySelectorAll("[data-flow-field]").forEach((f) => {
            f.classList.toggle("hidden", !show.includes(f.dataset.flowField));
        });
    }

    function wireRow(row) {
        applyOp(row);
        const op = row.querySelector("[data-flow-op]");
        if (op) op.addEventListener("change", () => {
            applyOp(row);
            resetPlayback();
            refreshMarks();
        });
        // Restrict drag to the handle: arm draggable on grab, disarm after.
        const handle = row.querySelector("[data-flow-drag]");
        if (handle) {
            handle.addEventListener("mousedown", () => row.setAttribute("draggable", "true"));
            // A click that never became a drag still disarms; a real drag ends
            // via dragend (the browser suppresses mouseup mid-drag).
            handle.addEventListener("mouseup", () => row.removeAttribute("draggable"));
            row.addEventListener("dragend", () => row.removeAttribute("draggable"));
        }
    }

    function addRow() {
        if (!tmpl) return null;
        const frag = tmpl.content.cloneNode(true);
        const row = frag.querySelector("[data-flow-row]");
        wrap.appendChild(frag);
        wireRow(row);
        // The op <select> in the clone template is never in the live DOM, so the
        // combobox load-scan skips it; enhance this fresh copy explicitly.
        window.smInitComboboxes?.(row);
        renumber();
        resetPlayback();
        return row;
    }

    if (addBtn) addBtn.addEventListener("click", addRow);

    wrap.addEventListener("click", (evt) => {
        const rm = evt.target.closest("[data-flow-remove]");
        if (!rm) return;
        if (rows().length <= 1) return; // keep at least one step
        rm.closest("[data-flow-row]").remove();
        renumber();
        resetPlayback();
    });

    // Any edit to the step list invalidates the last test's per-step verdicts.
    wrap.addEventListener("input", (evt) => {
        // Editing a flagged row is the author dealing with the warning.
        const row = evt.target.closest("[data-flow-row]");
        if (row) row.classList.remove("flow-step--flagged");
        resetPlayback();
        refreshMarks();
    });

    // Drag reorder.
    let dragging = null;
    wrap.addEventListener("dragstart", (evt) => {
        const row = evt.target.closest("[data-flow-row]");
        if (!row || row.getAttribute("draggable") !== "true") return;
        dragging = row;
        row.classList.add("opacity-50");
        evt.dataTransfer.effectAllowed = "move";
    });
    wrap.addEventListener("dragover", (evt) => {
        if (!dragging) return;
        evt.preventDefault();
        const over = evt.target.closest("[data-flow-row]");
        if (!over || over === dragging) return;
        const rect = over.getBoundingClientRect();
        const after = evt.clientY > rect.top + rect.height / 2;
        wrap.insertBefore(dragging, after ? over.nextSibling : over);
    });
    wrap.addEventListener("dragend", () => {
        if (dragging) dragging.classList.remove("opacity-50");
        dragging = null;
        renumber();
        resetPlayback();
    });

    // --- Test-now playback -------------------------------------------------
    // On Test-now the rows walk optimistically (pass → pass → running…) at an
    // estimated cadence; when the real result lands, reconcilePlayback snaps
    // every row to the true verdict. The engine returns one result, so the walk
    // is a progress illusion — truth always wins on reconcile.
    const STEP_STATES = ["flow-step--pass", "flow-step--fail", "flow-step--run", "flow-step--skip"];
    const MARK_PASS = "✓";
    const MARK_FAIL = "✗";
    const MARK_RUN = '<span class="flow-step__spin">⟳</span>';
    const WALK_MS = 600;
    let walkTimer = null;

    function clearRow(row) {
        row.classList.remove(...STEP_STATES);
        const st = row.querySelector("[data-flow-status]");
        if (st) st.textContent = "";
        const rs = row.querySelector("[data-flow-reason]");
        if (rs) {
            rs.textContent = "";
            rs.classList.add("hidden");
        }
    }

    function setRow(row, state, marker, reason) {
        row.classList.remove(...STEP_STATES);
        row.classList.add(state);
        const st = row.querySelector("[data-flow-status]");
        if (st) st.innerHTML = marker;
        const rs = row.querySelector("[data-flow-reason]");
        if (rs) {
            if (reason) {
                rs.textContent = reason;
                rs.classList.remove("hidden");
            } else {
                rs.textContent = "";
                rs.classList.add("hidden");
            }
        }
    }

    function resetPlayback() {
        if (walkTimer) {
            clearTimeout(walkTimer);
            walkTimer = null;
        }
        rows().forEach(clearRow);
    }

    function startPlayback() {
        resetPlayback();
        const list = rows();
        if (list.length === 0) return;
        let running = 0;
        setRow(list[0], "flow-step--run", MARK_RUN);
        const advance = () => {
            // Keep the last row spinning until the real result arrives rather
            // than optimistically passing it.
            if (running >= list.length - 1) {
                walkTimer = null;
                return;
            }
            setRow(list[running], "flow-step--pass", MARK_PASS);
            running += 1;
            setRow(list[running], "flow-step--run", MARK_RUN);
            walkTimer = setTimeout(advance, WALK_MS);
        };
        walkTimer = setTimeout(advance, WALK_MS);
    }

    function reconcilePlayback(result) {
        if (walkTimer) {
            clearTimeout(walkTimer);
            walkTimer = null;
        }
        const list = rows();
        if (list.length === 0) return;
        const status = (result && result.status) || "";
        const err = (result && result.error) || "";
        if (status === "up") {
            list.forEach((row) => setRow(row, "flow-step--pass", MARK_PASS));
            return;
        }
        const m = err.match(/^step (\d+)\/\d+ /);
        const failIdx = m ? parseInt(m[1], 10) - 1 : -1;
        if (status === "down" && failIdx >= 0 && failIdx < list.length) {
            const reason = err.replace(/^step \d+\/\d+ \S+: /, "");
            list.forEach((row, i) => {
                if (i < failIdx) setRow(row, "flow-step--pass", MARK_PASS);
                else if (i === failIdx) setRow(row, "flow-step--fail", MARK_FAIL, reason);
                else setRow(row, "flow-step--skip", "");
            });
            return;
        }
        // Engine error (startup/timeout) or an unparseable verdict: no trustworthy
        // per-step boundary — clear the walk and let the result banner speak.
        resetPlayback();
    }

    window.smFlowTestStart = startPlayback;
    window.smFlowTestResult = reconcilePlayback;
    window.smFlowTestReset = resetPlayback;

    const val = (row, sel) => {
        const el = row.querySelector(sel);
        return el ? el.value : "";
    };
    const trimmed = (row, sel) => val(row, sel).trim();

    function setField(row, sel, value) {
        const el = row.querySelector(sel);
        if (el && value != null) el.value = value;
    }

    // Fields the API rejects when blank. `fill.value` is absent on purpose: an
    // empty one is legal, and it is how a dropped password waits to be filled.
    const REQUIRED_FIELDS = {
        goto: ["url"],
        click: ["selector"],
        fill: ["selector"],
        wait_for: ["selector"],
        assert_text: ["contains"],
        assert_url: ["contains"],
    };
    const FIELD_INPUT = {
        url: "[data-flow-url]",
        selector: "[data-flow-selector]",
        contains: "[data-flow-contains]",
    };

    function blankRequired(row) {
        const op = row.querySelector("[data-flow-op]").value;
        return (REQUIRED_FIELDS[op] || [])
            .map((f) => row.querySelector(FIELD_INPUT[f]))
            .filter((el) => el && el.value.trim() === "");
    }

    function markRow(row, bad) {
        row.classList.toggle("flow-step--incomplete", bad.length > 0);
        row.querySelectorAll("[aria-invalid]").forEach((el) => el.removeAttribute("aria-invalid"));
        bad.forEach((el) => el.setAttribute("aria-invalid", "true"));
    }

    // Only ever clears. Marks are raised at import time, where we know a step
    // arrived unusable; a row the author is still typing into is not a fault.
    function refreshMarks() {
        rows().forEach((row) => {
            if (row.classList.contains("flow-step--incomplete") && blankRequired(row).length === 0) {
                markRow(row, []);
            }
        });
    }

    // Items are {op, url, selector, value, contains}; absent fields stay blank.
    window.smFlowReplaceSteps = function (steps) {
        if (!Array.isArray(steps) || steps.length === 0) return 0;
        rows().forEach((row) => row.remove());
        steps.forEach((s) => {
            const row = addRow();
            if (!row) return;
            const op = row.querySelector("[data-flow-op]");
            if (op && OP_FIELDS[s.op]) {
                op.value = s.op;
                // The combobox only redraws its label on change, and the
                // event also drives applyOp.
                op.dispatchEvent(new Event("change", { bubbles: true }));
            }
            setField(row, "[data-flow-url]", s.url);
            setField(row, "[data-flow-selector]", s.selector);
            setField(row, "[data-flow-value]", s.value);
            setField(row, "[data-flow-contains]", s.contains);
        });
        renumber();
        resetPlayback();
        // Flag what the import could not complete here, rather than letting the
        // API reject it after a round-trip.
        rows().forEach((row) => markRow(row, blankRequired(row)));
        return rows().length;
    };

    // Rows an import warned about. Not invalid — the API would take them — so
    // they carry no aria-invalid, only the edge that says read this one.
    window.smFlowFlagRows = function (numbers) {
        const want = new Set(numbers);
        rows().forEach((row, i) => row.classList.toggle("flow-step--flagged", want.has(i + 1)));
    };

    // Ordered array of {op, ...fields} in DOM order, consumed by check_form.js.
    window.smCollectFlowSteps = function () {
        return rows().map((row) => {
            const op = row.querySelector("[data-flow-op]").value;
            switch (op) {
                case "goto":
                    return { op, url: trimmed(row, "[data-flow-url]") };
                case "click":
                    return { op, selector: trimmed(row, "[data-flow-selector]") };
                case "fill":
                    // Do not trim the value — a secret may carry meaningful
                    // whitespace, and the "***" sentinel must round-trip exactly.
                    return {
                        op,
                        selector: trimmed(row, "[data-flow-selector]"),
                        value: val(row, "[data-flow-value]"),
                    };
                case "wait_for":
                    return { op, selector: trimmed(row, "[data-flow-selector]") };
                case "assert_text": {
                    const selector = trimmed(row, "[data-flow-selector]");
                    return {
                        op,
                        selector: selector === "" ? null : selector,
                        contains: trimmed(row, "[data-flow-contains]"),
                    };
                }
                case "assert_url":
                    return { op, contains: trimmed(row, "[data-flow-contains]") };
                default:
                    return { op };
            }
        });
    };

    // Init last: wiring an empty create form calls addRow → resetPlayback, which
    // reads the playback state declared above, so it must run after that state
    // is initialised (not in the temporal dead zone).
    rows().forEach(wireRow);
    if (rows().length === 0) addRow();
    renumber();
})();
