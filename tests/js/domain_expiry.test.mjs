import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

// A deliberately small DOM double tests the authored script without adding a
// browser dependency. It does not claim to verify layout or browser rendering.
class Element {
    constructor(tag = "div") {
        this.tagName = tag;
        this.children = [];
        this.dataset = {};
        this.attributes = {};
        this.value = "";
        this.textContent = "";
    }
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; }
    setAttribute(key, value) { this.attributes[key] = value; }
    addEventListener(name, fn) { this[name] = fn; }
    focus() { this.focused = true; }
}

const source = readFileSync(new URL("../../assets/js/marketing/domain_expiry.js", import.meta.url), "utf8")
    .replace(/^import .*;\n/, "");

function harness(fetcher) {
    const ids = Object.fromEntries(["domain-form", "domain-name", "domain-submit", "domain-result"].map(id => [id, new Element()]));
    ids["domain-form"].dataset.probe = "/tools/domain-expiry-checker/probe";
    ids["domain-name"].value = "https://app.example.com/path?secret=private";
    const events = [];
    const context = vm.createContext({
        document: { getElementById: id => ids[id], createElement: tag => new Element(tag), createDocumentFragment: () => new Element("fragment") },
        fetch: fetcher, URL, AbortController, setTimeout, clearTimeout,
        toolUsed: (...args) => events.push(args), toolError: (...args) => events.push(args),
    });
    vm.runInContext(source, context);
    return { context, ids, events };
}

const answer = {
    ok: true, domain: "example.com", days_remaining: 90, expired: false,
    expires_at: "2027-01-01T00:00:00+00:00", checked_at: "2026-10-03T00:00:00+00:00",
    registrar: "<img src=x onerror=alert(1)>", source_url: "https://rdap.example.com/domain/example.com",
};
const nodes = root => [root, ...root.children.flatMap(nodes)];

test("success uses the normalized domain for setup and inserts registry text safely", async () => {
    const h = harness(async () => ({ ok: true, status: 200, json: async () => answer }));
    await vm.runInContext("run()", h.context);
    const rendered = nodes(h.ids["domain-result"]);
    assert(rendered.some(n => n.tagName === "dd" && n.textContent === answer.registrar));
    assert(!rendered.some(n => n.tagName === "img"));
    assert(rendered.some(n => n.href === "/start?kind=domain_expiry&url=example.com"));
    assert.equal(h.ids["domain-submit"].disabled, false);
    assert.equal(h.ids["domain-form"].attributes["aria-busy"], "false");
    assert(!JSON.stringify(h.events).includes("example.com"));
    assert(!JSON.stringify(h.events).includes("private"));
});

test("simultaneous submissions are ignored while loading", async () => {
    let complete;
    let calls = 0;
    const h = harness(() => { calls++; return new Promise(resolve => { complete = resolve; }); });
    const first = vm.runInContext("run()", h.context);
    assert.equal(h.ids["domain-submit"].disabled, true);
    assert.equal(h.ids["domain-form"].attributes["aria-busy"], "true");
    await vm.runInContext("run()", h.context);
    assert.equal(calls, 1);
    complete({ ok: true, status: 200, json: async () => answer });
    await first;
    assert.equal(h.ids["domain-submit"].disabled, false);
});

test("unknown expiry, rate limits, bad JSON, and network errors restore the form", async () => {
    for (const fetcher of [
        async () => ({ ok: true, status: 200, json: async () => ({ ok: false, error: "No public expiry. Check your registrar." }) }),
        async () => ({ ok: false, status: 429, json: async () => ({ error: "Wait a minute." }) }),
        async () => ({ ok: true, status: 200, json: async () => { throw new Error("HTML"); } }),
        async () => { throw new Error("offline"); },
        async () => { const error = new Error("timeout"); error.name = "AbortError"; throw error; },
        async () => ({ ok: true, status: 200, json: async () => ({ ...answer, expires_at: "not a date" }) }),
    ]) {
        const h = harness(fetcher);
        await vm.runInContext("run()", h.context);
        assert.equal(h.ids["domain-submit"].disabled, false);
        assert(nodes(h.ids["domain-result"]).some(n => n.textContent.length > 0));
        assert(!nodes(h.ids["domain-result"]).some(n => n.href?.startsWith("/start")));
    }
});

test("less than a day remaining differs from an expiry that already passed", () => {
    const h = harness(() => {});
    assert.equal(vm.runInContext("verdict({ days_remaining: 0, expired: false })", h.context), "less than one day left");
    assert.equal(vm.runInContext("verdict({ days_remaining: 0, expired: true })", h.context), "expiry date has passed");
});

test("unsafe source links are omitted", async () => {
    const h = harness(async () => ({ ok: true, status: 200, json: async () => ({ ...answer, source_url: "javascript:alert(1)" }) }));
    await vm.runInContext("run()", h.context);
    assert(!nodes(h.ids["domain-result"]).some(n => n.href?.startsWith("javascript:")));
});
