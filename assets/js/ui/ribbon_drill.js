// Uptime ribbon: click a failing cell to drill its bucket's raw checks into a
// drawer. A wide bucket can hold thousands of rows, so the drawer shows the
// bucket's true failing count up front and pages the list (load more) rather
// than dumping every row. Rows come from a server partial that reuses the
// recent-results row markup (with region), so the expand behaviour and styling
// match that table for free.

(function () {
    // Mirror the server's page size so the offset advances correctly.
    const CHECK_PAGE = 30;

    const ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
    function esc(s) {
        return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ESCAPES[c]);
    }

    // Per-open drill state, so "load more" knows where it is.
    let active = null;

    function drawer() {
        return document.getElementById("ribbon-drawer");
    }

    function clearActive() {
        document
            .querySelectorAll(".dashboard-ribbon__seg--active")
            .forEach((el) => el.classList.remove("dashboard-ribbon__seg--active"));
    }

    // Ring the open cell by its window, not a stored node: the 60s live poll
    // OOB-replaces the ribbon, so the original cell reference goes stale.
    function ringActive() {
        clearActive();
        if (!active) return;
        const cell = document.querySelector(
            `.dashboard-ribbon__seg--drill[data-from="${active.from}"][data-to="${active.to}"]`,
        );
        if (cell) cell.classList.add("dashboard-ribbon__seg--active");
    }

    function close() {
        const d = drawer();
        if (d) d.setAttribute("hidden", "");
        clearActive();
        active = null;
    }

    function noteRow(text, cls) {
        return `<tr><td colspan="6" class="px-3 py-3 font-mono text-xs ${cls}">${esc(text)}</td></tr>`;
    }

    async function loadPage() {
        if (!active) return;
        const d = drawer();
        const rows = d.querySelector("[data-ribbon-drawer-rows]");
        const foot = d.querySelector("[data-ribbon-drawer-foot]");
        const shown = d.querySelector("[data-ribbon-drawer-shown]");
        const moreBtn = d.querySelector("[data-ribbon-load-more]");
        const first = active.offset === 0;
        if (moreBtn) moreBtn.disabled = true;
        if (first) rows.innerHTML = noteRow("# loading…", "text-quiet");

        const qs = new URLSearchParams({ from: active.from, to: active.to, offset: String(active.offset) });
        if (active.region) qs.set("region", active.region);
        const url = `${active.url}?${qs.toString()}`;
        try {
            const r = await fetch(url, { headers: { Accept: "text/html" } });
            if (!r.ok) throw new Error(`HTTP ${r.status}`);
            const html = await r.text();
            const hasMore = r.headers.get("x-sm-has-more") === "true";
            // One <time> per check row (detail rows have none) → exact count.
            const rowCount = (html.match(/<time /g) || []).length;
            // The partial emits a "# no results" row when empty; on the first
            // page that stands in for the empty state, so just swap it in.
            if (first) rows.innerHTML = html;
            else rows.insertAdjacentHTML("beforeend", html);
            active.offset += CHECK_PAGE;
            active.loaded += rowCount;
            if (foot) foot.toggleAttribute("hidden", !hasMore);
            if (shown) shown.textContent = hasMore ? `showing ${active.loaded}` : "";
        } catch (err) {
            const msg = `could not load checks: ${String(err.message || err)}`;
            if (first) rows.innerHTML = noteRow(msg, "flash-text flash-text--bad");
            else if (shown) shown.textContent = msg;
        } finally {
            if (moreBtn) moreBtn.disabled = false;
        }
    }

    function open(cell) {
        const d = drawer();
        if (!d) return;
        active = {
            url: d.dataset.checksUrl,
            region: d.dataset.region || "",
            from: cell.dataset.from,
            to: cell.dataset.to,
            offset: 0,
            loaded: 0,
        };
        ringActive();
        d.removeAttribute("hidden");

        const title = d.querySelector("[data-ribbon-drawer-title]");
        const scale = d.querySelector("[data-ribbon-drawer-scale]");
        if (title) {
            const label = window.smRibbonTipLabel
                ? window.smRibbonTipLabel(cell)
                : cell.dataset.tipTime;
            title.textContent = `${label} · ${cell.dataset.tipStat}`;
        }
        if (scale) {
            const total = parseInt(cell.dataset.total, 10) || 0;
            const bad = parseInt(cell.dataset.bad, 10) || 0;
            scale.textContent = bad
                ? `${bad.toLocaleString()} of ${total.toLocaleString()} checks failing`
                : `${total.toLocaleString()} checks`;
        }
        loadPage();
    }

    document.body.addEventListener("click", (ev) => {
        if (ev.target.closest("[data-ribbon-drawer-close]")) {
            close();
            return;
        }
        if (ev.target.closest("[data-ribbon-load-more]")) {
            loadPage();
            return;
        }
        const cell = ev.target.closest("[data-ribbon-drill]");
        if (!cell) return;
        if (active && cell.dataset.from === active.from && cell.dataset.to === active.to) {
            close();
        } else {
            open(cell);
        }
    });

    document.addEventListener("keydown", (ev) => {
        if (ev.key === "Escape") close();
    });

    // Live poll re-renders the ribbon; restore the open cell's highlight.
    document.body.addEventListener("htmx:afterSettle", () => {
        if (active) ringActive();
    });
})();
