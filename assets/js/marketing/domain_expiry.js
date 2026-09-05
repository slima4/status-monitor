import { toolError, toolUsed } from "./_tool_event.js";

const TOOL = "domain-expiry-checker";
const form = document.getElementById("domain-form");
const input = document.getElementById("domain-name");
const button = document.getElementById("domain-submit");
const out = document.getElementById("domain-result");
let busy = false;

if (form && input && button && out) {
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        run();
    });
}

function el(tag, cls, text) {
    const node = document.createElement(tag);
    node.className = cls;
    if (text !== undefined) node.textContent = text;
    return node;
}

function message(text) {
    out.replaceChildren(el("p", "tool-dns__empty mk-mono", text));
}

async function run() {
    if (busy) return;
    const domain = input.value.trim();
    if (!domain || domain.length > 2048) {
        message("Enter a domain name, such as example.com.");
        input.focus();
        return;
    }
    busy = true;
    button.disabled = true;
    button.textContent = "checking…";
    form.setAttribute("aria-busy", "true");
    message("Reading public registration data…");
    // Never send the submitted domain, URL, or registry response to analytics.
    toolUsed(TOOL);
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 25000);
    try {
        const res = await fetch(`${form.dataset.probe}?domain=${encodeURIComponent(domain)}`, {
            headers: { accept: "application/json" }, signal: controller.signal,
        });
        let body;
        try { body = await res.json(); } catch { body = null; }
        if (!res.ok || body?.ok !== true) {
            message(typeof body?.error === "string" ? body.error : "The checker returned an unreadable response. Please try again.");
            toolError(TOOL, { reason: `status-${res.status}` });
            return;
        }
        if (!validReport(body)) {
            message("The registry result could not be read. Check your registrar account for the renewal date.");
            toolError(TOOL, { reason: "malformed-body" });
            return;
        }
        out.replaceChildren(render(body));
    } catch (error) {
        message(error.name === "AbortError"
            ? "The lookup took too long. Try again later or check your registrar account."
            : "Could not reach the checker. Check your connection and try again.");
        toolError(TOOL, { reason: error.name === "AbortError" ? "timeout" : "probe-unreachable" });
    } finally {
        clearTimeout(timer);
        busy = false;
        button.disabled = false;
        button.textContent = "check expiry";
        form.setAttribute("aria-busy", "false");
    }
}

function validReport(r) {
    return typeof r.domain === "string" && r.domain.length > 0
        && Number.isInteger(r.days_remaining) && typeof r.expired === "boolean"
        && typeof r.expires_at === "string" && Number.isFinite(Date.parse(r.expires_at))
        && typeof r.checked_at === "string" && Number.isFinite(Date.parse(r.checked_at))
        && (r.registrar === null || typeof r.registrar === "string");
}

function timestamp(iso) {
    return new Date(iso).toISOString().replace("T", " ").replace(/\.\d{3}Z$/, " UTC");
}

function verdict(r) {
    const days = Math.abs(r.days_remaining);
    const unit = days === 1 ? "day" : "days";
    if (r.expired) return days === 0 ? "expiry date has passed" : `expiry date passed ${days} ${unit} ago`;
    return days === 0 ? "less than one day left" : `${days} ${unit} left`;
}

function render(r) {
    const fragment = document.createDocumentFragment();
    fragment.append(el("p", "tool-dns__head mk-mono", `registration for ${r.domain}`));
    const tone = r.expired ? "dead" : r.days_remaining <= 7 ? "critical" : r.days_remaining <= 30 ? "warn" : "ok";
    const status = el("p", `tool-ssl__verdict mk-mono tool-ssl__verdict--${tone}`);
    status.append(el("span", "tool-ssl__days", verdict(r)));
    status.append(el("span", "tool-ssl__until", `registry expiry: ${timestamp(r.expires_at)}`));
    fragment.append(status);

    const facts = el("dl", "tool-ssl__facts");
    for (const [label, value] of [["registered domain", r.domain], ["registrar", r.registrar || "not provided"], ["checked at", timestamp(r.checked_at)]]) {
        facts.append(el("dt", "tool-metric__label", label));
        facts.append(el("dd", "tool-ssl__value mk-mono", value));
    }
    fragment.append(facts);
    try {
        const source = new URL(r.source_url);
        if (source.protocol === "https:" && !source.username && !source.password) {
            const link = el("a", "mk-link", "view registry response");
            link.href = source.href;
            link.target = "_blank";
            link.rel = "noopener noreferrer";
            fragment.append(link);
        }
    } catch { /* A malformed source link must not hide the date. */ }
    fragment.append(el("p", "tool-dns__warn mk-mono", r.expired
        ? "The reported expiry date has passed. Contact your registrar. This does not mean the domain is available to buy."
        : "Confirm the renewal deadline and payment in your registrar account. A future registry date is not proof of payment."));
    const cta = el("a", "mk-cta mk-cta--primary tool-dns__cta", "monitor this domain");
    cta.href = `/start?kind=domain_expiry&url=${encodeURIComponent(r.domain)}`;
    cta.dataset.umamiEvent = "signup-start";
    cta.dataset.umamiEventPosition = "tool-domain-result";
    fragment.append(cta);
    return fragment;
}
