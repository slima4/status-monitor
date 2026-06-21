// "Who's on call now" readout on /settings/on-call. The schedule list is
// swapped in by HTMX, so resolution runs after each settle: for every schedule
// row it asks GET /api/v1/on-call/who and names the returned user ids from the
// hidden member map rendered alongside the list.
(function () {
    function memberMap(root) {
        const map = {};
        root.querySelectorAll("[data-member-id]").forEach((el) => {
            map[el.getAttribute("data-member-id")] = el.getAttribute("data-member-email");
        });
        return map;
    }

    async function resolveOne(cell, map) {
        const id = cell.getAttribute("data-schedule-id");
        try {
            const res = await fetch(
                "/api/v1/on-call/who?schedule_id=" + encodeURIComponent(id),
                { headers: { Accept: "application/json", "X-Requested-With": "uptimepage" } },
            );
            if (!res.ok) { cell.textContent = "—"; cell.className = "text-quiet"; return; }
            const body = await res.json();
            const ids = (body && body.user_ids) || [];
            if (ids.length === 0) {
                cell.textContent = "no one";
                cell.className = "text-quiet";
                return;
            }
            cell.textContent = ids.map((u) => map[u] || u).join(", ");
            cell.className = "text-body";
        } catch {
            cell.textContent = "—";
            cell.className = "text-quiet";
        }
    }

    function refresh(container) {
        const map = memberMap(container);
        container.querySelectorAll("[data-who-on-call]").forEach((cell) => resolveOne(cell, map));
    }

    document.body.addEventListener("htmx:afterSettle", (evt) => {
        if (evt.target && evt.target.id === "on-call-list") refresh(evt.target);
    });
})();
