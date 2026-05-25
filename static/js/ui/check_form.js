// Monitor create/edit form. Handles:
//   - 5-way protocol panel swap (http / tcp / dns / tls_cert / domain_expiry)
//   - Smart expected-status input ("200" / "200-299" / "200, 201, 204")
//   - JSON submission via fetch (no htmx for JSON-API forms)
//   - "Test now" → POST /api/v1/targets/test, renders verbose response inline
//   - Cmd/Ctrl+Enter to submit
// Headers are gathered via window.smCollectHeaders (header_rows.js).
// Tags are gathered via window.smCollectTags (tag_chip_input.js).

(function () {
    const form = document.getElementById("check-form");
    if (!form) return;

    const PROTOCOL_VARIANTS = ["http", "tcp", "dns", "tls_cert", "domain_expiry"];

    const DEFAULT_INTERVAL = {
        http: 60, tcp: 60, dns: 60, tls_cert: 86400, domain_expiry: 86400,
    };
    const KIND_MIN_INTERVAL = {
        http: 10, tcp: 10, dns: 10, tls_cert: 3600, domain_expiry: 3600,
    };

    const intervalInput = document.getElementById("interval_s");
    if (intervalInput) {
        if (form.dataset.mode !== "create") {
            intervalInput.dataset.userTouched = "1";
        }
        intervalInput.addEventListener("input", () => {
            intervalInput.dataset.userTouched = "1";
        });
    }

    function applyKindIntervalDefaults(kind) {
        if (!intervalInput) return;
        const planMin = Number(form.dataset.minInterval) || 60;
        const effectiveMin = Math.max(planMin, KIND_MIN_INTERVAL[kind] || 10);
        intervalInput.min = String(effectiveMin);
        if (intervalInput.dataset.userTouched !== "1") {
            intervalInput.value = String(Math.max(effectiveMin, DEFAULT_INTERVAL[kind] || 60));
        }
    }

    form.addEventListener("change", (evt) => {
        if (evt.target.name !== "check_type") return;
        const want = evt.target.value;
        document.querySelectorAll("[data-variant]").forEach(el => {
            const v = el.dataset.variant;
            if (PROTOCOL_VARIANTS.includes(v)) {
                el.classList.toggle("hidden", v !== want);
            } else if (v === "http-advanced") {
                el.classList.toggle("hidden", want !== "http");
            }
        });
        applyKindIntervalDefaults(want);
    });

    const initialKind = form.querySelector("input[name='check_type']:checked")?.value || "http";
    applyKindIntervalDefaults(initialKind);

    form.addEventListener("click", (evt) => {
        const preset = evt.target.closest("[data-interval-preset]");
        if (preset) {
            const input = document.getElementById("interval_s");
            if (input) {
                input.value = preset.dataset.intervalPreset;
                input.dataset.userTouched = "1";
                input.focus({ preventScroll: true });
            }
            return;
        }
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

    // ARIA radiogroup keyboard nav on the 5 check-type cards. Labels are
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

    function urlToName(raw) {
        if (!raw) return "";
        try {
            // Accept a bare host like `example.com` by adding a scheme.
            const u = new URL(/^[a-z]+:\/\//i.test(raw) ? raw : `https://${raw}`);
            return u.hostname.replace(/^www\./i, "");
        } catch {
            return "";
        }
    }

    form.addEventListener("submit", async (evt) => {
        evt.preventDefault();
        clearErrors();
        const built = buildBody();
        if (built.error) { renderClientError(built.error); return; }
        // SubmitEvent.submitter is the actual clicked button (null for
        // form.requestSubmit() / Cmd+Enter, which we treat as primary save).
        const submitter = evt.submitter;
        const saveAndTest = submitter && submitter.hasAttribute("data-save-and-test");

        let res;
        try {
            res = await fetch(form.dataset.action, {
                method: form.dataset.method,
                headers: jsonHeaders(),
                body: JSON.stringify(built.payload),
            });
        } catch (err) {
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
            if (saveAndTest && id) {
                // Best-effort: scheduler will run a check anyway on its own.
                await window.smRunCheckNow(id);
            }
            window.location = id ? `/targets/${id}` : "/targets";
            return;
        }

        let body;
        try { body = await res.json(); }
        catch { renderClientError(`Request failed (${res.status})`); return; }
        renderApiError(body, res.status);
    });

    function jsonHeaders() {
        return {
            "Content-Type": "application/json",
            "Accept": "application/json",
            "X-Requested-With": "status-monitor",
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
            const headers = (window.smCollectHeaders && window.smCollectHeaders()) || {};
            const expectedRaw = (data.get("expected_status_input") || "").trim();
            const expected = parseExpectedStatus(expectedRaw);
            if (expected.error) return { error: expected.error };

            const check = {
                type: "http",
                url: data.get("http_url"),
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

            const basicFs = document.querySelector("[data-auth-field='basic']");
            const bearerFs = document.querySelector("[data-auth-field='bearer']");
            if (basicFs && basicFs.dataset.mode === "replacing") {
                check.basic_auth = [
                    data.get("http_basic_user") || "",
                    data.get("http_basic_pass") || "",
                ];
            }
            if (bearerFs && bearerFs.dataset.mode === "replacing") {
                check.bearer_token = data.get("http_bearer_token") || "";
            }
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
        const built = buildCheck();
        if (built.error) return { error: built.error };
        const check = built.check;

        const tags = (window.smCollectTags && window.smCollectTags()) || [];

        const alerts = [];
        for (const row of form.querySelectorAll("[data-channel-row]")) {
            const cb = row.querySelector("[data-channel-select]");
            if (!cb || !cb.checked) continue;
            const after = parseInt(row.querySelector("[data-after-failures]").value, 10);
            if (!Number.isInteger(after) || after < 1) {
                return { error: `"After N failures" for channel must be a whole number ≥ 1.` };
            }
            alerts.push({
                channel_id: cb.value,
                after_failures: after,
                notify_recovery: row.querySelector("[data-notify-recovery]").checked,
            });
        }

        const planMin = Number(form.dataset.minInterval) || 60;
        const kind = data.get("check_type") || "http";
        const minInterval = Math.max(planMin, KIND_MIN_INTERVAL[kind] || 10);
        const interval = parseInt(data.get("interval_s"), 10);
        if (!Number.isInteger(interval) || interval < minInterval) {
            return { error: `Check interval must be at least ${minInterval} seconds.` };
        }

        const groupRaw = (data.get("group_name") || "").trim();
        const ownerRaw = (data.get("owner_user_id") || "").trim();
        const payload = {
            name: data.get("name"),
            interval,
            enabled: data.get("enabled") === "on",
            tags,
            check,
            alerts,
        };
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

    async function handleTestNow(btn) {
        const panel = btn.closest("[data-variant]");
        const resultEl = panel ? panel.querySelector("[data-test-result]") : null;
        const built = buildCheck();
        if (built.error) {
            renderClientError(built.error);
            return;
        }
        btn.disabled = true;
        window.smRenderCheckRunning(resultEl);
        try {
            let res;
            try {
                res = await fetch("/api/v1/targets/test", {
                    method: "POST",
                    headers: jsonHeaders(),
                    body: JSON.stringify({ check: built.check }),
                });
            } catch (err) {
                window.smRenderCheckError(resultEl, `Network error: ${err.message || err}`);
                return;
            }
            let body;
            try { body = await res.json(); } catch { body = null; }
            if (!res.ok) {
                const code = (body && body.error && body.error.code) || `HTTP ${res.status}`;
                const message = (body && body.error && body.error.message) || "Test rejected.";
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
        } finally {
            btn.disabled = false;
        }
    }

    function blankToNull(v) { return v && v.length > 0 ? v : null; }

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
            onField: (field) => {
                const target = fieldForApiPath(field);
                if (target) {
                    target.setAttribute("aria-invalid", "true");
                    target.focus({ preventScroll: true });
                    target.scrollIntoView({ block: "center", behavior: "smooth" });
                }
            },
        });
    }

    const API_PATH_TO_FORM = {
        "check.url": "http_url",
        "check.basic_auth": "http_basic_user",
        "check.bearer_token": "http_bearer_token",
        "check.verify_tls": "http_verify_tls",
        "check.body": "http_body",
        "check.expected_status": "expected_status_input",
        "check.host": null, // protocol-dependent; resolved at runtime below
        "check.port": null,
        "check.timeout": null,
        "check.domain": null,
        "check.record_type": "dns_record_type",
        "check.resolver": "dns_resolver",
        "check.expected_contains": "dns_expected_contains",
        "check.server_name": "tls_server_name",
        "check.warn_days": null,
        "check.critical_days": null,
        "interval": "interval_s",
        "name": "name",
        "tags": null,
    };

    function fieldForApiPath(path) {
        // Some API field paths map to a different form input depending on
        // which protocol is selected (e.g. `check.host` is `tcp_host` or
        // `tls_host`). Resolve those here.
        const checkType = currentCheckType();
        if (path === "check.host") {
            return form.querySelector(`[name='${checkType === "tls_cert" ? "tls_host" : "tcp_host"}']`);
        }
        if (path === "check.port") {
            return form.querySelector(`[name='${checkType === "tls_cert" ? "tls_port" : "tcp_port"}']`);
        }
        if (path === "check.domain") {
            return form.querySelector(`[name='${checkType === "domain_expiry" ? "domain_expiry_domain" : "dns_domain"}']`);
        }
        if (path === "check.timeout") {
            const prefix = checkType === "tls_cert"
                ? "tls"
                : checkType === "domain_expiry"
                    ? "domain_expiry"
                    : checkType;
            return form.querySelector(`[name='${prefix}_timeout_ms']`);
        }
        if (path === "check.warn_days" || path === "check.critical_days") {
            const prefix = checkType === "domain_expiry" ? "domain_expiry_" : "tls_";
            const suffix = path === "check.warn_days" ? "warn_days" : "critical_days";
            return form.querySelector(`[name='${prefix}${suffix}']`);
        }
        if (path.startsWith("check.headers")) {
            return form.querySelector("[name='http_header_key']");
        }
        if (path === "tags") {
            return form.querySelector("[data-tag-input]");
        }
        const name = API_PATH_TO_FORM[path];
        if (!name) return null;
        return form.querySelector(`[name="${name}"]`);
    }
})();
