// DNS lookup over DoH, straight from the visitor's browser to two public
// resolvers. Nothing reaches our servers, and the browser is the vantage point
// that actually matters: a resolver that lies to the visitor is invisible from
// ours.
import { toolError, toolUsed } from "./_tool_event.js";

const TOOL = "dns-lookup";

const RESOLVERS = [
    { label: "cloudflare", endpoint: "https://cloudflare-dns.com/dns-query" },
    { label: "google", endpoint: "https://dns.google/resolve" },
];

// Only the codes a visitor can act on; anything else is reported by number.
const STATUS = {
    0: "no error",
    2: "server failure",
    3: "no such name",
    5: "refused",
};

const form = document.getElementById("dns-form");
const nameInput = document.getElementById("dns-name");
const out = document.getElementById("dns-result");

if (form && nameInput && out) {
    form.addEventListener("submit", (e) => {
        e.preventDefault();
        run();
    });
}

// Strips a pasted scheme and path so "https://acme.com/pricing" still resolves.
function cleanName(raw) {
    let s = raw.trim().replace(/^[a-z][a-z0-9+.-]*:\/\//i, "");
    s = s.split("/")[0].split("?")[0].split("#")[0];
    s = s.replace(/\.$/, "");
    return s;
}

function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text !== undefined) n.textContent = text;
    return n;
}

function replace(node) {
    out.replaceChildren(node);
}

async function ask(resolver, name, type) {
    const url = `${resolver.endpoint}?name=${encodeURIComponent(name)}&type=${encodeURIComponent(type)}`;
    const res = await fetch(url, { headers: { accept: "application/dns-json" } });
    if (!res.ok) throw new Error(`${resolver.label} returned ${res.status}`);
    const body = await res.json();
    return {
        label: resolver.label,
        status: body.Status ?? 0,
        answers: (body.Answer ?? [])
            .filter((a) => Number(a.type) === Number(type))
            .map((a) => ({ data: String(a.data ?? ""), ttl: Number(a.TTL ?? 0) })),
    };
}

async function run() {
    const name = cleanName(nameInput.value);
    const picked = form.querySelector('input[name="type"]:checked');
    const type = picked?.value ?? "1";
    const typeLabel = picked?.dataset.label ?? "A";

    if (!name || !name.includes(".")) {
        replace(el("p", "tool-dns__empty mk-mono", "That does not look like a domain name."));
        toolError(TOOL, { reason: "malformed-name" });
        return;
    }

    replace(el("p", "tool-dns__empty mk-mono", "Looking up…"));
    toolUsed(TOOL, { mode: typeLabel });

    let results;
    try {
        results = await Promise.all(RESOLVERS.map((r) => ask(r, name, type)));
    } catch {
        replace(
            el(
                "p",
                "tool-dns__empty mk-mono",
                "Could not reach a resolver. Check your connection and try again.",
            ),
        );
        toolError(TOOL, { reason: "resolver-unreachable" });
        return;
    }

    replace(render(name, typeLabel, results, type));
}

// Addresses are steered by the asking resolver's location, so two CDN-fronted
// answers differ without anything being wrong. Everything else should match.
const LOCATION_STEERED = new Set([1, 28]);
// TXT carries DKIM keys and verification tokens, where case is significant.
const TXT = 16;

// JSON, not a join: TXT answers are free text and can contain any separator,
// so ["ab","c"] and ["ab|c"] must not collapse onto the same key.
function answerKey(r, type) {
    const norm = (d) => (Number(type) === TXT ? d : d.toLowerCase());
    return JSON.stringify(r.answers.map((a) => norm(a.data)).sort());
}

function same(results, fn) {
    return new Set(results.map(fn)).size === 1;
}

// Status is compared alongside the records because two resolvers can both
// return nothing, one because the name does not exist and one because it is
// being filtered, and telling those apart is the whole point of asking twice.
// TTLs are left out: they legitimately differ.
function verdict(results, type) {
    const statusAgrees = same(results, (r) => r.status);
    const answersAgree = same(results, (r) => answerKey(r, type));
    if (statusAgrees && answersAgree) return null;
    // Only a pure address difference is routine. A status split is never that,
    // whatever the record type, because one resolver is answering and the
    // other is not.
    if (statusAgrees && LOCATION_STEERED.has(Number(type))) {
        return "The two resolvers got different addresses. That is expected behind a CDN or geo-routed name, and worth reading otherwise.";
    }
    return "The two resolvers disagree. Either a change is still spreading, or one of them is being filtered.";
}

function render(name, typeLabel, results, type) {
    const frag = document.createDocumentFragment();

    const head = el("p", "tool-dns__head mk-mono");
    head.append(el("span", "tool-dns__q", `${typeLabel} ${name}`));
    frag.append(head);

    const grid = el("div", "tool-dns__grid");
    for (const r of results) {
        const col = el("div", "tool-dns__col");
        col.append(el("p", "tool-metric__label", r.label));

        if (r.status !== 0) {
            col.append(el("p", "tool-dns__none mk-mono", STATUS[r.status] ?? `status ${r.status}`));
        } else if (r.answers.length === 0) {
            col.append(el("p", "tool-dns__none mk-mono", "no records"));
        } else {
            const list = el("ul", "tool-dns__records");
            for (const a of r.answers) {
                const li = el("li");
                li.append(el("span", "tool-dns__data mk-mono", a.data));
                li.append(el("span", "tool-dns__ttl mk-mono", `ttl ${a.ttl}`));
                list.append(li);
            }
            col.append(list);
        }
        grid.append(col);
    }
    frag.append(grid);

    const warning = verdict(results, type);
    if (warning) {
        frag.append(el("p", "tool-dns__warn mk-mono", warning));
    }

    const cta = el("a", "mk-cta mk-cta--primary tool-dns__cta", "monitor this record");
    cta.href = `/start?kind=dns&url=${encodeURIComponent(name)}`;
    cta.dataset.umamiEvent = "signup-start";
    cta.dataset.umamiEventPosition = "tool-dns-result";
    frag.append(cta);

    return frag;
}
