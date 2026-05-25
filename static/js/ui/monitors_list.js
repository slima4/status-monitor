// Monitors page: bulk-select, group-collapse persistence, '/' hotkey,
// per-row ⋯ popover menu. Vanilla JS, event-delegated so handlers
// survive #target-rows swaps.
(function () {
  const ROOT = document;
  const STORAGE_KEY = "sm:monitors:groups:collapsed";

  function persistedCollapsed() {
    try { return JSON.parse(localStorage.getItem(STORAGE_KEY)) || {}; }
    catch { return {}; }
  }
  function saveCollapsed(state) {
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(state)); }
    catch { /* private mode — non-fatal */ }
  }
  function wireGroupPersistence() {
    const state = persistedCollapsed();
    ROOT.querySelectorAll("details.monitors-group").forEach(d => {
      const key = d.dataset.groupName || "";
      if (state[key]) d.removeAttribute("open");
      d.addEventListener("toggle", () => {
        const s = persistedCollapsed();
        if (d.open) delete s[key]; else s[key] = true;
        saveCollapsed(s);
      });
    });
  }

  function selectedIds() {
    return Array.from(ROOT.querySelectorAll("[data-row-select]:checked"))
      .map(cb => cb.closest("[data-row-id]").dataset.rowId);
  }
  function refreshBulkBar() {
    const bar = ROOT.getElementById("monitors-bulk");
    if (!bar) return;
    const count = selectedIds().length;
    const label = bar.querySelector("[data-bulk-count]");
    if (label) label.textContent = count;
    bar.dataset.empty = count === 0 ? "true" : "false";
    ROOT.querySelectorAll("[data-row-id]").forEach(row => {
      const cb = row.querySelector("[data-row-select]");
      row.dataset.selected = (cb && cb.checked) ? "true" : "false";
    });
  }
  function wireBulkSelect() {
    ROOT.addEventListener("change", e => {
      const t = e.target;
      if (t.matches("[data-row-select]")) refreshBulkBar();
      else if (t.matches("[data-group-select]")) {
        const tbl = t.closest("details").querySelector(".monitors-table");
        tbl.querySelectorAll("[data-row-select]").forEach(cb => { cb.checked = t.checked; });
        refreshBulkBar();
      }
    });
    ROOT.addEventListener("click", e => {
      const clear = e.target.closest("[data-bulk-clear]");
      if (clear) {
        ROOT.querySelectorAll("[data-row-select]:checked").forEach(cb => { cb.checked = false; });
        refreshBulkBar();
        return;
      }
      const action = e.target.closest("[data-bulk-action]");
      if (action) {
        e.preventDefault();
        runBulkAction(action);
      }
    });
  }
  async function runBulkAction(btn) {
    const bar = ROOT.getElementById("monitors-bulk");
    if (!bar) return;
    const ids = selectedIds();
    if (ids.length === 0) return;
    const action = btn.dataset.bulkAction;
    if (btn.dataset.bulkConfirm) {
      const ok = await window.smConfirm({
        title: "Bulk action",
        body: btn.dataset.bulkConfirm,
        confirmLabel: action === "delete" ? "Delete" : "Continue",
        danger: action === "delete",
      });
      if (!ok) return;
    }
    let body = { ids, action: { type: action } };
    if (btn.dataset.bulkPrompt) {
      const input = await window.smPrompt({
        title: "Bulk action",
        body: btn.dataset.bulkPrompt,
        splitOnComma: btn.dataset.bulkPromptSplit !== undefined,
      });
      if (input === null) return;
      body.action[btn.dataset.bulkPromptKey] = input;
    }
    const url = bar.dataset.actionUrl || "/api/v1/targets/bulk-action";
    try {
      const r = await fetch(url, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Accept": "application/json",
          "X-Requested-With": "status-monitor",
        },
        body: JSON.stringify(body),
      });
      if (!r.ok) {
        window.smToast({ message: "Bulk action failed: " + r.status });
        return;
      }
      const refresh = bar.dataset.refreshUrl;
      const rows = ROOT.getElementById("target-rows");
      if (window.htmx && refresh && rows) {
        window.htmx.ajax("GET", refresh, { target: "#target-rows", swap: "outerHTML" });
      } else {
        window.location.reload();
      }
    } catch (err) {
      window.smToast({ message: "Network error: " + err.message });
    }
  }

  // `position: fixed` escapes the group-card's `overflow: hidden`.
  let openMenu = null;
  function closeOpenMenu() {
    if (!openMenu) return;
    const root = openMenu;
    openMenu = null;
    root.dataset.open = "false";
    const panel = root.querySelector("[data-menu-panel]");
    const btn = root.querySelector("[data-menu-toggle]");
    if (panel) panel.hidden = true;
    if (btn) btn.setAttribute("aria-expanded", "false");
  }
  function positionMenu(root) {
    const btn = root.querySelector("[data-menu-toggle]");
    const panel = root.querySelector("[data-menu-panel]");
    if (!btn || !panel) return;
    const rect = btn.getBoundingClientRect();
    // Show hidden first so offsetWidth measures.
    panel.hidden = false;
    panel.style.visibility = "hidden";
    const w = panel.offsetWidth || 176;
    const h = panel.offsetHeight || 200;
    let top = rect.bottom + 4;
    let left = rect.right - w;
    if (top + h > window.innerHeight - 8) {
      top = Math.max(8, rect.top - h - 4);
    }
    if (left < 8) left = 8;
    panel.style.top = top + "px";
    panel.style.left = left + "px";
    panel.style.visibility = "";
  }
  function openMenuFor(root) {
    if (openMenu === root) {
      closeOpenMenu();
      return;
    }
    closeOpenMenu();
    openMenu = root;
    root.dataset.open = "true";
    const btn = root.querySelector("[data-menu-toggle]");
    if (btn) btn.setAttribute("aria-expanded", "true");
    positionMenu(root);
  }
  function wireRowMenu() {
    ROOT.addEventListener("click", e => {
      const toggle = e.target.closest("[data-menu-toggle]");
      if (toggle) {
        e.preventDefault();
        const root = toggle.closest("[data-menu-root]");
        if (root) openMenuFor(root);
        return;
      }
      const action = e.target.closest("[data-row-action]");
      if (action && openMenu && openMenu.contains(action)) {
        e.preventDefault();
        runRowAction(action);
        return;
      }
      if (openMenu && !openMenu.contains(e.target)) closeOpenMenu();
    });
    ROOT.addEventListener("keydown", e => {
      if (e.key === "Escape" && openMenu) {
        closeOpenMenu();
        const btn = e.target.closest("[data-menu-root]")?.querySelector("[data-menu-toggle]");
        if (btn) btn.focus();
      }
    });
    window.addEventListener("scroll", () => { if (openMenu) positionMenu(openMenu); }, true);
    window.addEventListener("resize", () => { if (openMenu) positionMenu(openMenu); });
  }
  async function runRowAction(btn) {
    const id = btn.dataset.targetId;
    const action = btn.dataset.rowAction;
    if (!id || !action) return;
    if (btn.dataset.rowConfirm) {
      const ok = await window.smConfirm({
        title: action === "delete" ? "Delete monitor?" : "Confirm",
        body: btn.dataset.rowConfirm,
        confirmLabel: action === "delete" ? "Delete" : "Continue",
        danger: action === "delete",
      });
      if (!ok) return;
    }
    closeOpenMenu();
    const bar = ROOT.getElementById("monitors-bulk");
    const url = (bar && bar.dataset.actionUrl) || "/api/v1/targets/bulk-action";
    try {
      const r = await fetch(url, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "Accept": "application/json",
          "X-Requested-With": "status-monitor",
        },
        body: JSON.stringify({ ids: [id], action: { type: action } }),
      });
      if (!r.ok) {
        window.smToast({ message: "Action failed: " + r.status });
        return;
      }
      const refresh = bar && bar.dataset.refreshUrl;
      const rows = ROOT.getElementById("target-rows");
      if (window.htmx && refresh && rows) {
        window.htmx.ajax("GET", refresh, { target: "#target-rows", swap: "outerHTML" });
      } else {
        window.location.reload();
      }
    } catch (err) {
      window.smToast({ message: "Network error: " + err.message });
    }
  }

  function wireSearchHotkey() {
    ROOT.addEventListener("keydown", e => {
      if (e.key !== "/" || e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      const search = ROOT.getElementById("monitors-search");
      if (search) {
        e.preventDefault();
        search.focus();
        search.select();
      }
    });
  }

  function init() {
    wireGroupPersistence();
    wireBulkSelect();
    wireRowMenu();
    wireSearchHotkey();
    refreshBulkBar();
  }

  if (window.htmx) {
    document.body.addEventListener("htmx:afterSwap", e => {
      if (e.detail && e.detail.target && e.detail.target.id === "target-rows") {
        closeOpenMenu();
        wireGroupPersistence();
        refreshBulkBar();
      }
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
