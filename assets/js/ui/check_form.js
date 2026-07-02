// Monitor create/edit form. Handles:
//   - protocol panel swap (http / tcp / ping / dns / tls_cert / domain_expiry)
//   - Smart expected-status input ("200" / "200-299" / "200, 201, 204")
//   - JSON submission via fetch (no htmx for JSON-API forms)
//   - "Test now" → POST /api/v1/targets/test, renders verbose response inline
//   - Cmd/Ctrl+Enter to submit
// Headers are gathered via window.smCollectHeaders (header_rows.js).
// Tags are gathered via window.smCollectTags (tag_chip_input.js).

(function () {
    const form = document.getElementById("check-form");
    if (!form) return;

    // Per-kind interval floors, mirrored from the API via data-kind-floors —
    // the slow/fast rail split and defaults all derive from them.
    let KIND_MIN_INTERVAL = {};
    try {
        KIND_MIN_INTERVAL = JSON.parse(form.dataset.kindFloors || "{}");
    } catch { /* validation falls back to the plan floor */ }
    const kindFloor = (kind) => KIND_MIN_INTERVAL[kind] || 10;
    const isSlowKind = (kind) => kindFloor(kind) >= 3600;

    // Remember each rail's last cadence so switching kinds back restores it
    // (checking a radio in one rail auto-unchecks the other's — shared name).
    form.querySelectorAll("[data-interval-rail]").forEach((rail) => {
        const cur = rail.querySelector("input[name='interval_s']:checked");
        if (cur) rail.dataset.last = cur.value;
    });

    function applyKindIntervalDefaults(kind) {
        const want = isSlowKind(kind) ? "slow" : "fast";
        let active = null;
        form.querySelectorAll("[data-interval-rail]").forEach((rail) => {
            const on = rail.dataset.intervalRail === want;
            rail.hidden = !on;
            if (on) active = rail;
        });
        if (!active || active.querySelector("input[name='interval_s']:checked")) return;
        const pick = (v) => active.querySelector(`input[name='interval_s'][value='${v}']`);
        const def = pick(active.dataset.last)
            || pick(isSlowKind(kind) ? 86400 : 60)
            || active.querySelector("input[name='interval_s']");
        if (def) def.checked = true;
    }

    form.addEventListener("change", (evt) => {
        if (evt.target.name === "interval_s") {
            const rail = evt.target.closest("[data-interval-rail]");
            if (rail) rail.dataset.last = evt.target.value;
            return;
        }
        if (evt.target.name !== "check_type") return;
        const want = evt.target.value;
        document.querySelectorAll("[data-variant]").forEach(el => {
            el.classList.toggle("hidden", el.dataset.variant !== want);
        });
        applyKindIntervalDefaults(want);
    });

    const initialKind = form.querySelector("input[name='check_type']:checked")?.value || "http";
    applyKindIntervalDefaults(initialKind);

    // Expected-status quick presets: a segmented rail that fills the text
    // field (the source of truth). A class preset writes its range; "custom"
    // jumps to the field. Typing anything off-preset lights "custom".
    const statusInput = form.querySelector("[data-status-input]");
    const statusPresets = form.querySelector("[data-status-presets]");
    if (statusInput && statusPresets) {
        const presetRadios = [...statusPresets.querySelectorAll("[data-status-preset]")];
        const norm = (v) => (v || "").replace(/\s+/g, "");
        const syncRailFromField = () => {
            const val = norm(statusInput.value);
            const match = presetRadios.find(r => r.value !== "custom" && norm(r.value) === val)
                || presetRadios.find(r => r.value === "custom");
            presetRadios.forEach(r => { r.checked = r === match; });
        };
        statusPresets.addEventListener("change", (evt) => {
            const r = evt.target.closest("[data-status-preset]");
            if (!r) return;
            if (r.value === "custom") {
                statusInput.focus();
                statusInput.select();
            } else {
                statusInput.value = r.value;
            }
        });
        statusInput.addEventListener("input", syncRailFromField);
        syncRailFromField();
    }

    form.addEventListener("click", (evt) => {
        const testBtn = evt.target.closest("[data-test-now]");
        if (testBtn) {
            handleTestNow(testBtn);
        }
    });

    form.addEventListener("keydown", (evt) => {
        if ((evt.metaKey || evt.ctrlKey) && evt.key === "Enter") {
            evt.preventDefault();
            form.requestSubmit();
        }
    });

    // ARIA radiogroup keyboard nav on the check-type cards. Labels are
    // tabindex-0 as a fallback for browsers that won't focus sr-only radios.
    const checkTypeCards = form.querySelector(".check-type-cards");
    if (checkTypeCards) {
        checkTypeCards.querySelectorAll("[data-check-card]").forEach(label => {
            if (!label.hasAttribute("tabindex")) label.setAttribute("tabindex", "0");
        });

        form.addEventListener("keydown", (evt) => {
            if (evt.key !== "ArrowLeft" && evt.key !== "ArrowRight"
                && evt.key !== "ArrowUp" && evt.key !== "ArrowDown") return;
            const active = document.activeElement;
            if (!active || !checkTypeCards.contains(active)) return;
            if (active.tagName === "INPUT" && active.type !== "radio") return;
            const radios = Array.from(checkTypeCards.querySelectorAll("input[name='check_type']"));
            if (radios.length === 0) return;
            const checkedIdx = radios.findIndex(r => r.checked);
            const delta = (evt.key === "ArrowLeft" || evt.key === "ArrowUp") ? -1 : 1;
            const next = checkedIdx < 0
                ? 0
                : (checkedIdx + delta + radios.length) % radios.length;
            evt.preventDefault();
            radios[next].checked = true;
            radios[next].dispatchEvent(new Event("change", { bubbles: true }));
            (radios[next].closest("[data-check-card]") || radios[next]).focus();
        });

        // Space/Enter selects the focused card.
        checkTypeCards.addEventListener("keydown", (evt) => {
            if (evt.key !== " " && evt.key !== "Enter") return;
            const label = evt.target.closest("[data-check-card]");
            if (!label) return;
            const radio = label.querySelector("input[name='check_type']");
            if (!radio || radio.checked) return;
            evt.preventDefault();
            radio.checked = true;
            radio.dispatchEvent(new Event("change", { bubbles: true }));
        });
    }

    // Auto-fill Name from the primary input until the user types into Name.
    const nameInput = form.querySelector("[name='name']");
    if (nameInput) {
        nameInput.addEventListener("input", () => {
            nameInput.dataset.userTouched = nameInput.value.trim() ? "1" : "";
        });
        const AUTOFILL_SOURCES = [
            ["http_url", urlToName],
            ["tcp_host", v => v.trim()],
            ["ping_host", v => v.trim()],
            ["dns_domain", v => v.trim()],
            ["tls_host", v => v.trim()],
            ["domain_expiry_domain", v => v.trim()],
        ];
        for (const [name, derive] of AUTOFILL_SOURCES) {
            const src = form.querySelector(`[name='${name}']`);
            if (!src) continue;
            src.addEventListener("input", () => {
                if (nameInput.dataset.userTouched) return;
                const derived = derive(src.value);
                if (derived) nameInput.value = derived;
            });
        }
    }

    // Treat a bare host as https:// — one definition shared by URL validation and name autofill.
    const SCHEME_RE = /^[a-z][a-z0-9+.-]*:\/\//i;
    const ensureScheme = (raw) => (SCHEME_RE.test(raw) ? raw : `https://${raw}`);

    // Fail a malformed URL locally; the server stays authoritative for scheme allow-listing and SSRF.
    function normalizeHttpUrl(raw) {
        const trimmed = (raw || "").trim();
        if (!trimmed) {
            return { error: "URL is required — enter the endpoint to monitor.", field: "check.url" };
        }
        const candidate = ensureScheme(trimmed);
        let u;
        try { u = new URL(candidate); }
        catch { return { error: "That doesn’t look like a valid URL.", field: "check.url" }; }
        if (u.protocol !== "http:" && u.protocol !== "https:") {
            return { error: "URL must start with http:// or https://.", field: "check.url" };
        }
        return { url: candidate, prepended: candidate !== trimmed };
    }

    function urlToName(raw) {
        if (!raw) return "";
        try {
            const u = new URL(ensureScheme(raw));
            return u.hostname.replace(/^www\./i, "");
        } catch {
            return "";
        }
    }

    // The title input auto-grows with field-sizing where available; mirror its
    // width manually for engines that lack it so the name never sits in a wide box.
    if (nameInput && !(window.CSS && CSS.supports && CSS.supports("field-sizing", "content"))) {
        const cs = getComputedStyle(nameInput);
        const mirror = document.createElement("span");
        mirror.setAttribute("aria-hidden", "true");
        mirror.style.cssText = "position:absolute;visibility:hidden;white-space:pre;";
        for (const p of ["fontFamily", "fontSize", "fontWeight", "letterSpacing"]) mirror.style[p] = cs[p];
        nameInput.insertAdjacentElement("afterend", mirror);
        const autosize = () => {
            mirror.textContent = nameInput.value || nameInput.placeholder || "";
            const cap = nameInput.parentElement.clientWidth || 9999;
            nameInput.style.width = `${Math.min(mirror.offsetWidth + 6, cap)}px`;
        };
        nameInput.addEventListener("input", autosize);
        autosize();
    }

    // Unsaved-changes guard: warn before leaving a dirtied form, but not while
    // the save itself is navigating away.
    let formDirty = false;
    let submitting = false;
    form.addEventListener("input", () => {
        formDirty = true;
        // Drop a shown validation error once the user starts correcting it.
        if (errorsShown()) clearErrors();
    });
    form.addEventListener("change", () => { formDirty = true; });
    // In-form links (cancel, back) are intentional exits — don't guard them.
    form.querySelectorAll("a[href]").forEach((a) => {
        a.addEventListener("click", () => { formDirty = false; });
    });
    window.addEventListener("beforeunload", (evt) => {
        if (formDirty && !submitting) { evt.preventDefault(); evt.returnValue = ""; }
    });

    form.addEventListener("submit", async (evt) => {
        evt.preventDefault();
        clearErrors();
        const built = buildBody();
        if (built.error) {
            renderClientError(built.error);
            if (built.field) markFieldInvalid(built.field);
            return;
        }
        // Detection threshold rides the payload when the region fieldset is present.
        // Symbolic values (any/majority/all) go as-is; a number becomes {count: n}.
        const regionRoot = document.querySelector("[data-monitor-regions]");
        if (regionRoot) {
            const sel = regionRoot.querySelector("[data-region-policy]");
            if (sel) {
                const n = parseInt(sel.value, 10);
                built.payload.region_policy = Number.isInteger(n) ? { count: n } : sel.value;
            }
        }

        // SubmitEvent.submitter is the actual clicked button (null for
        // form.requestSubmit() / Cmd+Enter, which we treat as primary save).
        const submitter = evt.submitter;

        const submitBtns = [...form.querySelectorAll("button[type='submit']")];
        const restoreLabel = submitter ? submitter.innerHTML : null;
        const setSubmitting = (on) => {
            submitting = on;
            submitBtns.forEach((b) => { b.disabled = on; });
            if (!submitter) return;
            if (on) {
                submitter.setAttribute("aria-busy", "true");
                submitter.innerHTML = form.dataset.mode === "create" ? "creating…" : "saving…";
            } else {
                submitter.removeAttribute("aria-busy");
                if (restoreLabel != null) submitter.innerHTML = restoreLabel;
            }
        };
        setSubmitting(true);

        let res;
        try {
            res = await fetch(form.dataset.action, {
                method: form.dataset.method,
                headers: jsonHeaders(),
                body: JSON.stringify(built.payload),
            });
        } catch (err) {
            setSubmitting(false);
            renderClientError(`Network error: ${err.message || err}`);
            return;
        }

        if (res.ok) {
            let id = null;
            try {
                const json = await res.json();
                id = json.id;
            } catch { /* PATCH may return 200 with body; create returns 201. */ }
            if (!id && form.dataset.mode === "edit") {
                const parts = form.dataset.action.split("/");
                id = parts[parts.length - 1];
            }
            // Apply the chosen regions (best-effort; on create the server
            // already seeded default coverage if this fails).
            if (regionRoot && id) {
                const regions = [...regionRoot.querySelectorAll("[data-region-checkbox]:checked")]
                    .map((c) => c.value);
                if (regions.length) {
                    try {
                        await fetch(`/api/v1/targets/${id}/regions`, {
                            method: "PUT",
                            headers: jsonHeaders(),
                            body: JSON.stringify({ regions }),
                        });
                    } catch { /* server default coverage stands */ }
                }
            }
            window.location = id ? `/targets/${id}` : "/targets";
            return;
        }

        setSubmitting(false);
        let body;
        try { body = await res.json(); }
        catch { renderClientError(`Request failed (${res.status})`); return; }
        renderApiError(body, res.status);
    });

    function jsonHeaders() {
        return {
            "Content-Type": "application/json",
            "Accept": "application/json",
            "X-Requested-With": "uptimepage",
        };
    }

    function currentCheckType() {
        const radio = form.querySelector("input[name='check_type']:checked");
        return radio ? radio.value : "http";
    }

    function buildCheck() {
        const data = new FormData(form);
        const checkType = currentCheckType();

        if (checkType === "http") {
            const norm = normalizeHttpUrl(data.get("http_url"));
            if (norm.error) return { error: norm.error, field: norm.field };
            if (norm.prepended) {
                const urlInput = form.querySelector("[name='http_url']");
                if (urlInput) {
                    urlInput.value = norm.url;
                    urlInput.dispatchEvent(new Event("input", { bubbles: true }));
                }
            }
            const headers = (window.smCollectHeaders && window.smCollectHeaders()) || {};
            const expectedRaw = (data.get("expected_status_input") || "").trim();
            const expected = parseExpectedStatus(expectedRaw);
            if (expected.error) return { error: expected.error };

            const check = {
                type: "http",
                url: norm.url,
                method: data.get("http_method") || "GET",
                timeout: parseInt(data.get("http_timeout_ms"), 10) || 5000,
                follow_redirects: data.get("http_follow_redirects") === "on",
                max_redirects: parseInt(data.get("http_max_redirects"), 10) || 5,
                expected_status: expected.value,
                expected_body_contains: blankToNull(data.get("http_expected_body_contains")),
                headers,
                body: blankToNull(data.get("http_body")),
                verify_tls: data.get("http_verify_tls") === "on",
            };
            return { check };
        }
        if (checkType === "tcp") {
            return {
                check: {
                    type: "tcp",
                    host: data.get("tcp_host"),
                    port: parseInt(data.get("tcp_port"), 10),
                    timeout: parseInt(data.get("tcp_timeout_ms"), 10),
                },
            };
        }
        if (checkType === "ping") {
            return {
                check: {
                    type: "ping",
                    host: data.get("ping_host"),
                    timeout: parseInt(data.get("ping_timeout_ms"), 10),
                },
            };
        }
        if (checkType === "dns") {
            return {
                check: {
                    type: "dns",
                    domain: data.get("dns_domain"),
                    record_type: data.get("dns_record_type"),
                    resolver: blankToNull(data.get("dns_resolver")),
                    expected_contains: blankToNull(data.get("dns_expected_contains")),
                    timeout: parseInt(data.get("dns_timeout_ms"), 10),
                },
            };
        }
        if (checkType === "tls_cert") {
            const warn = parseInt(data.get("tls_warn_days"), 10);
            const crit = parseInt(data.get("tls_critical_days"), 10);
            if (Number.isInteger(warn) && Number.isInteger(crit) && crit >= warn) {
                return { error: "Critical days must be less than warn days." };
            }
            return {
                check: {
                    type: "tls_cert",
                    host: data.get("tls_host"),
                    port: parseInt(data.get("tls_port"), 10),
                    server_name: blankToNull(data.get("tls_server_name")),
                    warn_days: warn,
                    critical_days: crit,
                    timeout: parseInt(data.get("tls_timeout_ms"), 10),
                },
            };
        }
        if (checkType === "domain_expiry") {
            const warn = parseInt(data.get("domain_expiry_warn_days"), 10);
            const crit = parseInt(data.get("domain_expiry_critical_days"), 10);
            if (Number.isInteger(warn) && Number.isInteger(crit) && crit >= warn) {
                return { error: "Critical days must be less than warn days." };
            }
            return {
                check: {
                    type: "domain_expiry",
                    domain: data.get("domain_expiry_domain"),
                    warn_days: warn,
                    critical_days: crit,
                    timeout: parseInt(data.get("domain_expiry_timeout_ms"), 10),
                },
            };
        }
        return { error: `Unknown check type: ${checkType}` };
    }

    function buildBody() {
        const data = new FormData(form);
        const name = (data.get("name") || "").trim();
        if (!name) {
            return { error: "Name is required — label this monitor so you can find it later.", field: "name" };
        }
        const built = buildCheck();
        if (built.error) return { error: built.error, field: built.field };
        const check = built.check;

        const tags = (window.smCollectTags && window.smCollectTags()) || [];

        const alerts = [];
        for (const row of form.querySelectorAll("[data-channel-row]")) {
            const cb = row.querySelector("[data-channel-select]");
            if (!cb || !cb.checked) continue;
            alerts.push({ channel_id: cb.value });
        }

        const confEl = form.querySelector("input[name='alert_confirmations']:checked");
        const confirmations = confEl ? parseInt(confEl.value, 10) : 2;
        if (!Number.isInteger(confirmations) || confirmations < 1) {
            return { error: "Open incident after must be a whole number of failed checks (≥ 1)." };
        }
        const recoveryEl = form.querySelector("[data-notify-recovery]");
        const renotifyEl = form.querySelector("[data-renotify-secs] input:checked");

        const planMin = Number(form.dataset.minInterval) || 60;
        const kind = data.get("check_type") || "http";
        const minInterval = Math.max(planMin, kindFloor(kind));
        const interval = parseInt(data.get("interval_s"), 10);
        if (!Number.isInteger(interval) || interval < minInterval) {
            return { error: `Check interval must be at least ${minInterval} seconds.` };
        }

        const groupRaw = (data.get("group_name") || "").trim();
        const ownerRaw = (data.get("owner_user_id") || "").trim();
        const payload = {
            name,
            interval,
            enabled: data.get("enabled") === "on",
            tags,
            check,
            alerts,
            alert_confirmations: confirmations,
        };
        // Absent with zero channels: create takes the server defaults,
        // edit (partial PATCH) keeps the stored values.
        if (recoveryEl) payload.notify_recovery = recoveryEl.checked;
        if (renotifyEl) payload.renotify_interval_secs = parseInt(renotifyEl.value, 10) || 0;
        payload.group_name = groupRaw === "" ? null : groupRaw;
        payload.owner_user_id = ownerRaw === "" ? null : ownerRaw;
        return { payload };
    }

    // "200" → Exact; "200-299" → Range; "200, 201, 204" → OneOf.
    function parseExpectedStatus(raw) {
        if (raw.length === 0) {
            return { error: "Expected status is required (e.g. 200 or 200-299)." };
        }
        let m;
        if ((m = /^(\d{3})$/.exec(raw))) {
            return { value: { kind: "exact", value: parseInt(m[1], 10) } };
        }
        if ((m = /^(\d{3})\s*-\s*(\d{3})$/.exec(raw))) {
            const min = parseInt(m[1], 10);
            const max = parseInt(m[2], 10);
            if (max < min) {
                return { error: "Expected status range max must be ≥ min." };
            }
            return { value: { kind: "range", value: { min, max } } };
        }
        if (/^[\d,\s]+$/.test(raw)) {
            const arr = raw
                .split(",")
                .map(s => s.trim())
                .filter(Boolean)
                .map(s => parseInt(s, 10));
            if (arr.length === 0 || arr.some(n => !Number.isInteger(n) || n < 100 || n > 599)) {
                return { error: "Expected status list must be HTTP codes (100–599)." };
            }
            if (arr.length === 1) {
                return { value: { kind: "exact", value: arr[0] } };
            }
            return { value: { kind: "one_of", value: arr } };
        }
        return {
            error: "Expected status must be like 200, 200-299, or 200, 201, 204.",
        };
    }

    // Run one test (optionally pinned to `region`) and render the outcome into
    // `resultEl` via the shared .test-result renderers.
    async function runTest(resultEl, check, region) {
        let res;
        try {
            res = await fetch("/api/v1/targets/test", {
                method: "POST",
                headers: jsonHeaders(),
                body: JSON.stringify(region ? { check, region } : { check }),
            });
        } catch (err) {
            window.smRenderCheckError(resultEl, `Network error: ${err.message || err}`);
            return;
        }
        if (res.status === 429) {
            const retry = res.headers.get("retry-after");
            window.smRenderCheckError(resultEl, retry
                ? `Rate limited — retry in ${retry}s`
                : "Rate limited — slow down");
            return;
        }
        // Read once as text, then try to parse: error responses are not always
        // our JSON envelope. A body-deserialize rejection (e.g. a missing
        // required field) comes back as axum's plain-text 422 — surface that
        // reason instead of a generic "rejected".
        const raw = await res.text();
        let body = null;
        try { body = raw ? JSON.parse(raw) : null; } catch { body = null; }
        if (!res.ok) {
            const code = (body && body.error && body.error.code) || `HTTP ${res.status}`;
            const detail = (body && body.error && body.error.message)
                || (raw && raw.trim())
                || "Test rejected.";
            const message = detail.length > 300 ? `${detail.slice(0, 300)}…` : detail;
            window.smRenderCheckError(resultEl, `${code}: ${message}`);
            return;
        }
        if (!body) {
            window.smRenderCheckError(resultEl, "Bad JSON in test response.");
            return;
        }
        window.smRenderCheckResult(resultEl, body.result || {}, {
            matched: body.matched_expectations,
            headers: body.response_headers_preview || [],
            body: body.response_body_snippet || null,
        });
    }

    async function handleTestNow(btn) {
        const resultEl = document.querySelector("[data-test-result]");
        clearErrors();
        const built = buildCheck();
        if (built.error) {
            renderClientError(built.error);
            if (built.field) markFieldInvalid(built.field);
            return;
        }
        // Fan out to every region selected on the form; the agent in each region
        // runs the probe.
        const allBoxes = [...form.querySelectorAll("[data-region-checkbox]")];
        const regions = allBoxes
            .filter((c) => c.checked)
            .map((c) => ({ id: c.value, label: (c.closest("label")?.textContent || c.value).trim() }));
        // Selector shown but every region deselected → an explicit "no regions"
        // choice; don't silently probe the default region.
        if (allBoxes.length > 0 && regions.length === 0) {
            window.smRenderCheckError(resultEl, "Select at least one region to test.");
            return;
        }
        btn.disabled = true;
        btn.setAttribute("aria-busy", "true");
        const btnLabel = btn.innerHTML;
        btn.innerHTML = "testing…";
        try {
            if (regions.length === 0) {
                // No region selector (single-region plan) → test the default region.
                window.smRenderCheckRunning(resultEl);
                await runTest(resultEl, built.check, null);
                return;
            }
            const rows = window.smRenderRegionTestRunning(resultEl, regions);
            await Promise.all(regions.map((r) => runTest(rows[r.id], built.check, r.id)));
        } finally {
            btn.disabled = false;
            btn.removeAttribute("aria-busy");
            btn.innerHTML = btnLabel;
        }
    }

    function blankToNull(v) { return v && v.length > 0 ? v : null; }

    function errorsShown() {
        const banner = document.getElementById("form-errors");
        return (banner && !banner.classList.contains("hidden")) || !!form.querySelector("[aria-invalid]");
    }

    // Flag a field as invalid and focus it; scroll into view only when the
    // error came from the server (the field may be far from the action).
    function markFieldInvalid(field, scroll) {
        const el = fieldForApiPath(field);
        if (!el) return;
        window.smRevealCollapsibleFor?.(el);
        window.smRevealOptionalFor?.(el);
        el.setAttribute("aria-invalid", "true");
        el.focus({ preventScroll: true });
        if (scroll) el.scrollIntoView({ block: "center", behavior: "smooth" });
    }

    function clearErrors() {
        window.smClearFormErrors(document.getElementById("form-errors"));
    }

    function renderClientError(msg) {
        window.smRenderClientError(document.getElementById("form-errors"), msg);
    }

    function renderApiError(json, status) {
        window.smRenderApiError(document.getElementById("form-errors"), json, status, {
            messageFor: (err) => err.code === "SSRF_BLOCKED"
                ? "This URL points to a private/internal range and is blocked. " +
                  "If you need to monitor internal services, deploy a monitor instance inside that network."
                : null,
            onField: (field) => markFieldInvalid(field, true),
        });
    }

    const API_PATH_TO_FORM = {
        "check.url": "http_url",
        "check.verify_tls": "http_verify_tls",
        "check.body": "http_body",
        "check.expected_status": "expected_status_input",
        "check.record_type": "dns_record_type",
        "check.resolver": "dns_resolver",
        "check.expected_contains": "dns_expected_contains",
        "check.server_name": "tls_server_name",
        "interval": "interval_s",
        "renotify_interval_secs": "renotify_secs",
        "name": "name",
    };

    // Protocol-dependent paths: inputs follow `<kind-ish prefix>_<suffix>` and
    // live inside their `fieldset[data-variant]`, so resolving by suffix
    // within the selected panel covers new kinds with no extra mapping.
    const API_PATH_SUFFIX = {
        "check.host": "_host",
        "check.port": "_port",
        "check.domain": "_domain",
        "check.timeout": "_timeout_ms",
        "check.warn_days": "_warn_days",
        "check.critical_days": "_critical_days",
    };

    function fieldForApiPath(path) {
        const suffix = API_PATH_SUFFIX[path];
        if (suffix) {
            return form.querySelector(
                `fieldset[data-variant='${currentCheckType()}'] [name$='${suffix}']`);
        }
        if (path.startsWith("check.headers")) {
            return form.querySelector("[name='http_header_key']");
        }
        if (path === "tags") {
            return form.querySelector("[data-tag-add]");
        }
        const name = API_PATH_TO_FORM[path];
        if (!name) return null;
        return form.querySelector(`[name="${name}"]`);
    }
})();
