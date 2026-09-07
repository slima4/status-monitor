// "Choose what to keep" on /settings/usage, shown only while the plan does not
// cover everything the account has. Submits the ticked ids to
// PUT /api/v1/account/holds; the server holds whatever is left over, so this
// form never deletes anything.
//
// A pick is authoritative: what is ticked runs, what is not is held, and
// keeping fewer than the plan seats is allowed. The only thing the form
// refuses is over-ticking, which the server cannot honour.
//
// Each fieldset saves under its own request field, and a fieldset that is not
// on the page sends nothing at all. That is what stops a picker showing only
// status pages from clearing the monitor choice made during an earlier
// shortage.
(function () {
    const form = document.querySelector("[data-holds-form]");
    if (!form) return;
    const msg = form.querySelector("[data-holds-msg]");
    const button = form.querySelector("button[type=submit]");
    const sets = Array.from(form.querySelectorAll("fieldset"));

    function show(text, kind) {
        if (!msg) return;
        msg.textContent = text;
        msg.className = "flash-text font-mono text-xs" + (kind ? " flash-text--" + kind : "");
    }

    function ticked(set, selector) {
        return set.querySelectorAll(selector);
    }

    // One budget per cap. A flow monitor spends a flow slot as well as an
    // ordinary one, so a pick can sit inside the monitor count and still be
    // more flows than the plan runs.
    function count(set) {
        const label = set.dataset.holdsLabel;
        const seats = Number(set.dataset.holdsSeats || "0");
        const kept = ticked(set, "input[type=checkbox]:checked").length;
        const out = { label, kept, over: [] };
        const main = set.querySelector("[data-holds-count]");
        if (main) {
            main.textContent = `${kept} of ${seats} kept`;
            main.classList.toggle("holds-count--over", kept > seats);
        }
        if (kept > seats) out.over.push(`${label}: ${kept - seats} too many`);

        if (set.dataset.holdsFlowSeats !== undefined) {
            const flowSeats = Number(set.dataset.holdsFlowSeats);
            const flowKept = ticked(set, "input[data-holds-flow]:checked").length;
            const el = set.querySelector("[data-holds-flow-count]");
            if (el) {
                el.textContent = `flows ${flowKept} of ${flowSeats}`;
                el.classList.toggle("holds-count--over", flowKept > flowSeats);
            }
            if (flowKept > flowSeats) {
                out.over.push(`flows: ${flowKept - flowSeats} too many`);
            }
        }
        return out;
    }

    function tally() {
        const over = [];
        const empty = [];
        for (const set of sets) {
            const c = count(set);
            over.push(...c.over);
            if (c.kept === 0) empty.push(c.label);
        }
        if (button) button.disabled = over.length > 0;
        if (over.length) {
            show(over.join(" · ") + " — untick before saving", "bad");
        } else if (empty.length) {
            // Not an error. An empty list is how you hand the choice back to
            // the plan, and saying so beats leaving it to be discovered.
            show(empty.join(", ") + ": nothing ticked — the plan keeps the oldest", null);
        } else {
            show("", null);
        }
        return over.length === 0;
    }

    form.addEventListener("change", tally);
    tally();

    let inFlight = false;
    form.addEventListener("submit", async (evt) => {
        evt.preventDefault();
        if (inFlight || !tally()) return;
        inFlight = true;
        if (button) button.disabled = true;
        show("saving…", "ok");
        const body = {};
        for (const set of sets) {
            body[set.dataset.holdsField] = Array.from(
                ticked(set, "input[type=checkbox]:checked"),
            ).map((c) => c.value);
        }
        try {
            const res = await fetch("/api/v1/account/holds", {
                method: "PUT",
                headers: {
                    "Content-Type": "application/json",
                    Accept: "application/json",
                    "X-Requested-With": "uptimepage",
                },
                body: JSON.stringify(body),
            });
            if (res.ok) {
                // Reload rather than patch the DOM: the save moves holds in
                // both directions, so the whole panel and every usage bar on
                // the page is stale, not just the rows that were ticked.
                window.location.reload();
                return;
            }
            show(await window.smApiErrorMessage(res, "could not save"), "bad");
        } catch (_e) {
            show("could not reach the server", "bad");
        } finally {
            inFlight = false;
            tally();
        }
    });
})();
