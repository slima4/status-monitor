// Imports a Chrome DevTools Recorder export, or a flow check as the API stores
// it, into the flow builder. Parsed in the page and never uploaded: a recording
// carries the password in clear text.
(function () {
    const form = document.getElementById("check-form");
    if (!form) return;
    const panel = document.getElementById("flow-import-panel");
    const toggle = form.querySelector("[data-flow-import-toggle]");
    if (!panel || !toggle) return;

    const file = panel.querySelector("[data-flow-import-file]");
    const text = panel.querySelector("[data-flow-import-text]");
    const apply = panel.querySelector("[data-flow-import-apply]");
    const notes = panel.querySelector("[data-flow-import-notes]");
    const startUrl = form.querySelector('[name="flow_start_url"]');

    // Mirrors FlowCheck::MAX_STEPS; the API rejects anything longer.
    const MAX_STEPS = 30;
    // Recorder selector flavours our engine cannot use; plain CSS starts with
    // none of them.
    const FOREIGN = ["xpath/", "text/", "aria/", "pierce/"];
    const SECRETISH = /pass|pwd|secret|token|otp|mfa|2fa|cvc|cvv|\bpin\b/i;

    function chains(selectors) {
        if (!Array.isArray(selectors)) return [];
        return selectors.map((c) => (Array.isArray(c) ? c : [c]));
    }

    // A multi-element chain is a shadow-DOM pierce path, which querySelector
    // cannot follow.
    function cssFrom(selectors) {
        for (const parts of chains(selectors)) {
            if (parts.length !== 1) continue;
            const s = String(parts[0] == null ? "" : parts[0]).trim();
            if (!s || FOREIGN.some((p) => s.startsWith(p))) continue;
            return s;
        }
        return "";
    }

    // A bare tag name matches the first such element on the page, which is
    // rarely the one that was recorded.
    const WEAK_SELECTOR = /^[a-zA-Z][a-zA-Z0-9]*$/;

    // Chrome's xpath candidate is often id-anchored where its CSS candidate is a
    // bare tag, so it converts into something far more precise. Bails on any
    // segment it does not fully understand rather than guessing.
    function cssFromXpath(selectors) {
        for (const parts of chains(selectors)) {
            if (parts.length !== 1) continue;
            const raw = String(parts[0] == null ? "" : parts[0]);
            if (!raw.startsWith("xpath/")) continue;
            const segments = raw.slice(6).replace(/^\/+/, "").split("/").filter(Boolean);
            const out = [];
            let ok = segments.length > 0;
            for (const seg of segments) {
                const byId = seg.match(/^\*\[@id="([^"]+)"\]$/);
                const indexed = seg.match(/^([a-zA-Z][\w-]*)\[(\d+)\]$/);
                if (byId) out.push(`#${byId[1]}`);
                else if (indexed) out.push(`${indexed[1]}:nth-of-type(${indexed[2]})`);
                else if (/^[a-zA-Z][\w-]*$/.test(seg)) out.push(seg);
                else { ok = false; break; }
            }
            if (ok) return out.join(" > ");
        }
        return "";
    }

    // Best available: a specific CSS selector, else one derived from the xpath,
    // else whatever plain CSS there was.
    function bestSelector(selectors) {
        const css = cssFrom(selectors);
        if (css && !WEAK_SELECTOR.test(css)) return css;
        return cssFromXpath(selectors) || css;
    }

    function looksSecret(selectors, selector) {
        const hay = [selector].concat(chains(selectors).flat()).join(" ");
        return SECRETISH.test(hay);
    }

    // Assert on the path: the query often carries a session id that would never
    // match the next run.
    function urlFragment(raw) {
        try {
            const u = new URL(raw);
            return u.pathname && u.pathname !== "/" ? u.pathname : raw;
        } catch (_) {
            return raw;
        }
    }

    function assertUrlFrom(step) {
        const events = Array.isArray(step.assertedEvents) ? step.assertedEvents : [];
        for (const e of events) {
            if (e && e.type === "navigation" && e.url) return urlFragment(e.url);
        }
        return "";
    }

    const OPS = ["goto", "fill", "click", "wait_for", "assert_text", "assert_url"];
    const HAS_VAR = /\{\{.+\}\}/;

    // A flow check as the API stores it. Steps come across as written: the
    // author chose them, so nothing is rewritten the way a recording is.
    function mapSpec(doc) {
        const warnings = [];
        const steps = [];

        doc.steps.forEach((s, i) => {
            const op = s && s.op;
            if (OPS.indexOf(op) < 0) {
                warnings.push({
                    text: `Step ${i + 1} has op "${op == null ? "" : op}", which is not a flow step. Dropped.`,
                    row: 0,
                });
                return;
            }
            const step = { op: op };
            for (const key of ["url", "selector", "value", "contains"]) {
                if (s[key] != null) step[key] = String(s[key]);
            }
            steps.push(step);
            if (op === "fill" && step.value && !HAS_VAR.test(step.value) && looksSecret([], step.selector || "")) {
                warnings.push({
                    text: `Row ${steps.length}: this looks like a password field holding a literal value. Point it at a secret variable instead.`,
                    row: steps.length,
                });
            }
        });

        return { start: doc.start_url == null ? "" : String(doc.start_url), steps: steps, warnings: warnings };
    }

    function mapRecording(doc) {
        const source = doc.steps;

        const warnings = [];
        const steps = [];
        let start = "";

        source.forEach((s, i) => {
            const n = i + 1;
            const type = s && s.type;
            const selector = bestSelector(s && s.selectors);
            let startedHere = false;

            let emitted = false;
            const emit = (step) => {
                emitted = true;
                steps.push(step);
            };
            // Warnings name the row on screen so it can be marked; steps that
            // produced no row fall back to the recording's own numbering.
            const rowWarn = (msg) => warnings.push({ text: `Row ${steps.length}: ${msg}`, row: steps.length });
            const dropWarn = (msg) => warnings.push({ text: `Recording step ${n} ${msg}`, row: 0 });
            const needsSelector = () => {
                if (!selector) {
                    rowWarn("no usable selector was recorded, only text or a shadow-DOM path. Fill it in by hand.");
                } else if (WEAK_SELECTOR.test(selector)) {
                    rowWarn(`the only selector recorded was the bare tag "${selector}", which matches the first one on the page. Make it specific before saving.`);
                }
            };

            switch (type) {
                case "navigate":
                    if (!start) {
                        start = s.url || "";
                        startedHere = true;
                    } else {
                        emit({ op: "goto", url: s.url || "" });
                    }
                    break;
                case "click":
                case "doubleClick":
                    emit({ op: "click", selector: selector });
                    needsSelector();
                    break;
                case "change": {
                    // Recorder logs the focus click before the typing; replaying
                    // both spends two of MAX_STEPS to do one thing. Dropped before
                    // the fill lands so quoted row numbers stay right.
                    const prev = steps[steps.length - 1];
                    if (prev && prev.op === "click" && prev.selector && prev.selector === selector) steps.pop();
                    if (looksSecret(s.selectors, selector)) {
                        emit({ op: "fill", selector: selector, value: "" });
                        rowWarn(
                            "the recorded value was dropped because the field looks like a password or token. Point it at a secret variable instead.",
                        );
                    } else {
                        emit({
                            op: "fill",
                            selector: selector,
                            value: s.value == null ? "" : String(s.value),
                        });
                    }
                    needsSelector();
                    break;
                }
                case "waitForElement":
                    emit({ op: "wait_for", selector: selector });
                    needsSelector();
                    break;
                case "keyDown":
                    if (s.key === "Enter") {
                        dropWarn("pressed Enter. There is no key step here — add a click on the submit control instead.");
                    }
                    break;
                case "keyUp":
                case "setViewport":
                case "scroll":
                case "hover":
                case "close":
                    break;
                case "waitForExpression":
                    dropWarn("waited on a JavaScript expression, which has no equivalent here. Dropped.");
                    break;
                default:
                    if (type) dropWarn(`is a "${type}" step, which is not supported. Dropped.`);
            }

            // Only a step that produced a row can name one. A dropped step
            // carrying a frame would otherwise mark whichever row came last.
            if (Array.isArray(s.frame) && s.frame.length > 0) {
                if (startedHere || !emitted) {
                    dropWarn("targets an iframe, which flow steps cannot reach.");
                } else {
                    rowWarn("this came from an iframe, which flow steps cannot reach. Check it before saving.");
                }
            }

            // Recorder hangs a navigation assertion off the step that caused it,
            // which is the success signal a flow needs. The one on the opening
            // navigate proves nothing.
            const fragment = startedHere ? "" : assertUrlFrom(s);
            const last = steps[steps.length - 1];
            if (fragment && !(last && last.op === "assert_url" && last.contains === fragment)) {
                steps.push({ op: "assert_url", contains: fragment });
            }
        });

        return { start: start, steps: steps, warnings: warnings };
    }

    // Both sources hit the same cap, and the same rule that without an
    // assertion the check can never fail.
    function settle(mapped, noun) {
        let kept = mapped.steps;
        if (kept.length > MAX_STEPS) {
            mapped.warnings.push({
                text: `This ${noun} produced ${kept.length} steps; only the first ${MAX_STEPS} were kept.`,
                row: 0,
            });
            kept = kept.slice(0, MAX_STEPS);
        }
        // The per-step reasons are the whole answer when nothing survived.
        if (kept.length === 0) {
            const why = mapped.warnings.map((w) => w.text).join(" ");
            throw new Error(`Nothing in this ${noun} maps to a flow step. ${why}`.trim());
        }
        if (!kept.some((s) => s.op === "assert_url" || s.op === "assert_text")) {
            mapped.warnings.push({
                text: `No assertion came out of this ${noun}. Add an assert step, or the check can never fail.`,
                row: 0,
            });
        }
        return { start: mapped.start, steps: kept, warnings: mapped.warnings, source: noun };
    }

    // A Recorder export describes what was done, with a `type` per action; a
    // flow check describes what to do, with an `op` per step.
    function mapImport(doc) {
        if (!doc || !Array.isArray(doc.steps)) {
            throw new Error('That JSON has no "steps" array, so it is neither a Recorder export nor a flow check.');
        }
        return doc.type === "flow" || doc.steps.some((s) => s && s.op)
            ? settle(mapSpec(doc), "flow check")
            : settle(mapRecording(doc), "Chrome recording");
    }

    // `lines` are {text, tone}: "" plain, "warn" needs-a-look, "bad" nothing
    // was imported.
    function report(lines) {
        notes.textContent = "";
        notes.classList.toggle("hidden", lines.length === 0);
        for (const line of lines) {
            const li = document.createElement("li");
            li.className = "flow-import__note" + (line.tone ? ` flow-import__note--${line.tone}` : "");
            li.textContent = line.text;
            notes.appendChild(li);
        }
    }

    const failure = (msg) => report([{ text: msg, tone: "bad" }]);

    // An import replaces every row, which on an edit form throws away hand-tuned
    // steps. Ask first, but only when there is real work to lose.
    function hasAuthoredSteps() {
        const rows = form.querySelectorAll("[data-flow-row]");
        if (rows.length > 1) return true;
        return Array.from(form.querySelectorAll("[data-flow-row] input")).some(
            (el) => el.value.trim() !== "",
        );
    }

    async function confirmReplace() {
        if (!hasAuthoredSteps()) return true;
        if (!window.smConfirm) return true;
        return window.smConfirm({
            title: "Replace the steps?",
            body: "Importing a recording discards the steps already in this form.",
            confirmLabel: "replace",
            danger: true,
        });
    }

    function run(raw) {
        let doc;
        try {
            doc = JSON.parse(raw);
        } catch (e) {
            failure(`That is not valid JSON: ${e.message}. Paste the whole file, braces included.`);
            return;
        }
        let mapped;
        try {
            mapped = mapImport(doc);
        } catch (e) {
            failure(e.message);
            return;
        }
        if (mapped.start && startUrl) startUrl.value = mapped.start;
        const count = window.smFlowReplaceSteps(mapped.steps);

        const warnings = mapped.warnings.slice();
        // A recording started on a page already open carries no navigation to
        // take the start URL from, and the API refuses a flow without one.
        if (startUrl && !startUrl.value.trim()) {
            warnings.unshift({
                text: `No start URL came out of this ${mapped.source}. Fill one in above before saving.`,
                row: 0,
            });
        }
        // Naming the format is how a paste read as the other one gets caught.
        const summary = {
            text: `Imported ${count} step${count === 1 ? "" : "s"} from a ${mapped.source}. Review them before saving.`,
            tone: "",
        };
        report([summary].concat(warnings.map((w) => ({ text: w.text, tone: "warn" }))));
        // A note that names a row marks it, so "Row 2" is something you can see
        // rather than something you count.
        if (window.smFlowFlagRows) window.smFlowFlagRows(warnings.map((w) => w.row).filter(Boolean));
    }

    toggle.addEventListener("click", () => {
        const open = toggle.getAttribute("aria-expanded") === "true";
        toggle.setAttribute("aria-expanded", open ? "false" : "true");
        panel.hidden = open;
    });

    if (file) {
        file.addEventListener("change", () => {
            const f = file.files && file.files[0];
            if (!f) return;
            f.text().then(
                async (raw) => {
                    if (await confirmReplace()) run(raw);
                },
                (e) => failure(`Could not read that file: ${e.message}`),
            );
            // Let the same file be picked again after a fix.
            file.value = "";
        });
    }

    if (apply) {
        apply.addEventListener("click", async () => {
            const raw = ((text && text.value) || "").trim();
            if (!raw) {
                failure("Paste the exported JSON first, or pick the file.");
                return;
            }
            if (await confirmReplace()) run(raw);
        });
    }
})();
