(function () {
    const form = document.getElementById("check-form");
    if (!form) return;

    const EXPECTED_VARIANT = {
        exact: "expected-exact",
        range: "expected-range",
        one_of: "expected-one-of",
    };

    form.addEventListener("change", (evt) => {
        const t = evt.target;
        if (t.name === "check_type") {
            document.querySelectorAll("[data-variant='http'], [data-variant='tcp']").forEach(el => {
                el.classList.toggle("hidden", el.dataset.variant !== t.value);
            });
        } else if (t.matches("[data-expected-toggle]")) {
            const want = EXPECTED_VARIANT[t.value];
            document.querySelectorAll("[data-variant^='expected-']").forEach(el => {
                el.classList.toggle("hidden", el.dataset.variant !== want);
            });
        }
    });

    form.addEventListener("submit", async (evt) => {
        evt.preventDefault();
        clearErrors();
        const built = buildBody();
        if (built.error) { renderClientError(built.error); return; }

        let res;
        try {
            res = await fetch(form.dataset.action, {
                method: form.dataset.method,
                headers: {
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "X-Requested-With": "status-monitor",
                },
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
            window.location = id ? `/targets/${id}` : "/targets";
            return;
        }

        let body;
        try { body = await res.json(); }
        catch { renderClientError(`Request failed (${res.status})`); return; }
        renderApiError(body, res.status);
    });

    function buildBody() {
        const data = new FormData(form);
        const checkType = data.get("check_type");
        let check;

        if (checkType === "http") {
            let headers;
            try {
                const raw = (data.get("http_headers") || "").trim();
                headers = raw ? JSON.parse(raw) : {};
                if (typeof headers !== "object" || headers === null || Array.isArray(headers)) {
                    throw new Error("not an object");
                }
            } catch {
                return { error: "Headers must be a JSON object — e.g. {\"Accept\": \"application/json\"}" };
            }

            const expectedKind = data.get("expected_kind") || "exact";
            let expected_status;
            if (expectedKind === "exact") {
                expected_status = {
                    kind: "exact",
                    value: parseInt(data.get("expected_exact"), 10),
                };
            } else if (expectedKind === "range") {
                expected_status = {
                    kind: "range",
                    value: {
                        min: parseInt(data.get("expected_range_min"), 10),
                        max: parseInt(data.get("expected_range_max"), 10),
                    },
                };
            } else {
                const arr = (data.get("expected_one_of") || "")
                    .split(",")
                    .map(s => s.trim())
                    .filter(Boolean)
                    .map(s => parseInt(s, 10));
                if (arr.some(Number.isNaN) || arr.length === 0) {
                    return { error: "Expected status 'one of' must be a comma-separated list of integers." };
                }
                expected_status = { kind: "one_of", value: arr };
            }

            check = {
                type: "http",
                url: data.get("http_url"),
                method: data.get("http_method"),
                timeout: parseInt(data.get("http_timeout_ms"), 10),
                follow_redirects: data.get("http_follow_redirects") === "on",
                max_redirects: parseInt(data.get("http_max_redirects"), 10),
                expected_status,
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
        } else {
            check = {
                type: "tcp",
                host: data.get("tcp_host"),
                port: parseInt(data.get("tcp_port"), 10),
                timeout: parseInt(data.get("tcp_timeout_ms"), 10),
            };
        }

        const tags = (data.get("tags") || "")
            .split(",")
            .map(s => s.trim())
            .filter(Boolean);

        const payload = {
            name: data.get("name"),
            interval: parseInt(data.get("interval_s"), 10),
            enabled: data.get("enabled") === "on",
            tags,
            check,
        };
        return { payload };
    }

    function blankToNull(v) { return v && v.length > 0 ? v : null; }

    function clearErrors() {
        const banner = document.getElementById("form-errors");
        banner.innerHTML = "";
        banner.classList.add("hidden");
        document.querySelectorAll("[aria-invalid]").forEach(el => el.removeAttribute("aria-invalid"));
    }

    function renderClientError(msg) {
        const banner = document.getElementById("form-errors");
        banner.textContent = msg;
        banner.classList.remove("hidden");
    }

    function renderApiError(json, status) {
        const banner = document.getElementById("form-errors");
        const err = (json && json.error) || {};
        const code = err.code || `HTTP ${status}`;
        let message = err.message || "Request rejected.";
        if (err.code === "SSRF_BLOCKED") {
            message = "This URL points to a private/internal range and is blocked. " +
                      "If you need to monitor internal services, deploy a monitor instance inside that network.";
        }
        banner.innerHTML = `<strong>${escapeHtml(code)}</strong>: ${escapeHtml(message)}`;
        if (err.field) {
            banner.insertAdjacentHTML("beforeend", ` <span class="text-xs text-red-600">(field: ${escapeHtml(err.field)})</span>`);
        }
        banner.classList.remove("hidden");
        banner.scrollIntoView({ block: "center", behavior: "smooth" });

        const target = err.field ? fieldForApiPath(err.field) : null;
        if (target) {
            target.setAttribute("aria-invalid", "true");
            target.focus({ preventScroll: true });
        }
    }

    const API_PATH_TO_FORM = {
        "check.url": "http_url",
        "check.basic_auth": "http_basic_user",
        "check.bearer_token": "http_bearer_token",
        "check.verify_tls": "http_verify_tls",
        "check.headers": "http_headers",
        "check.body": "http_body",
        "check.host": "tcp_host",
        "check.port": "tcp_port",
        "check.timeout": "http_timeout_ms",
        "interval": "interval_s",
        "name": "name",
        "tags": "tags",
    };

    function fieldForApiPath(path) {
        const name = API_PATH_TO_FORM[path];
        if (!name) return null;
        return form.querySelector(`[name="${name}"]`);
    }

    function escapeHtml(s) {
        return String(s).replace(/[&<>"']/g, ch => ({
            "&": "&amp;",
            "<": "&lt;",
            ">": "&gt;",
            "\"": "&quot;",
            "'": "&#39;",
        }[ch]));
    }
})();
