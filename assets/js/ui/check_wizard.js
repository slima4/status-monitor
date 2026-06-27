// Stepped create flow for the monitor form: target -> schedule & alerts.
// Edit mode renders every step at once, so this stays inert there.
// Position-independent — check_form.js owns all field logic; this only shows
// and hides [data-step] sections and drives the stepper/nav buttons.
(function () {
    const form = document.getElementById("check-form");
    if (!form || form.dataset.mode !== "create") return;

    const steps = [...form.querySelectorAll("[data-step]")];
    if (steps.length === 0) return;
    const total = steps.length;
    const tabs = [...form.querySelectorAll("[data-step-tab]")];
    const backBtn = form.querySelector("[data-wizard-back]");
    const nextBtn = form.querySelector("[data-wizard-next]");

    // Primary target field(s) per check type — the one thing step 1 must hold
    // before advancing. Format/range validation stays in check_form.js.
    const TARGET_FIELDS = {
        http: ["http_url"],
        tcp: ["tcp_host", "tcp_port"],
        dns: ["dns_domain"],
        tls_cert: ["tls_host"],
        domain_expiry: ["domain_expiry_domain"],
    };

    let cur = 1;

    function show(n) {
        cur = Math.min(Math.max(n, 1), total);
        steps.forEach((s) => s.classList.toggle("hidden", Number(s.dataset.step) !== cur));
        tabs.forEach((t) => {
            const i = Number(t.dataset.stepTab);
            t.classList.toggle("wizard-step--active", i === cur);
            t.classList.toggle("wizard-step--done", i < cur);
            if (i === cur) t.setAttribute("aria-current", "step");
            else t.removeAttribute("aria-current");
        });
        if (backBtn) backBtn.hidden = cur === 1;
        if (nextBtn) nextBtn.hidden = cur === total;
    }

    function targetFilled() {
        const type = form.querySelector("input[name='check_type']:checked")?.value || "http";
        for (const name of TARGET_FIELDS[type] || []) {
            const el = form.querySelector(`[name='${name}']`);
            if (el && !el.value.trim()) {
                el.setAttribute("aria-invalid", "true");
                el.focus();
                return false;
            }
        }
        return true;
    }

    function goForward(to) {
        if (cur === 1 && !targetFilled()) return;
        show(to);
    }

    nextBtn?.addEventListener("click", () => goForward(cur + 1));
    backBtn?.addEventListener("click", () => show(cur - 1));
    tabs.forEach((t) => t.addEventListener("click", () => {
        const i = Number(t.dataset.stepTab);
        if (i <= cur) show(i);
        else goForward(i);
    }));

    // Submit-time validation in check_form.js focuses an invalid field; if it
    // lives on a hidden step, surface that step first so the focus is visible.
    window.smRevealStepFor = (el) => {
        const sec = el?.closest("[data-step]");
        if (sec) show(Number(sec.dataset.step));
    };

    show(1);
})();
