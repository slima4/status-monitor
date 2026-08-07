// Footer badge rail. The strip only scrolls once this runs: the markup holds
// one set of badges, and the copies that make the loop seamless are cloned
// here. Speed is derived from the measured set so the rail keeps the same
// pixels-per-second whatever gets added to it.
const SPEED = 45;
const MAX_COPIES = 12;

const rail = document.querySelector("[data-badge-rail]");
const calm = matchMedia("(prefers-reduced-motion: reduce)");

const build = () => {
    const track = rail.querySelector(".mk-badges__track");
    const set = track.firstElementChild;

    track.replaceChildren(set);
    rail.classList.remove("is-live");
    if (calm.matches) return;

    // Flush the removal, so a rebuild restarts the marquee instead of dropping
    // a running animation onto a different endpoint mid-travel.
    void rail.offsetWidth;

    // Cloning happens under .is-live so the set is measured unwrapped.
    rail.classList.add("is-live");
    const cycle = set.getBoundingClientRect().width;
    if (!cycle) {
        rail.classList.remove("is-live");
        return;
    }

    const copies = Math.min(MAX_COPIES, Math.max(2, Math.ceil(rail.clientWidth / cycle) + 1));
    for (let i = 1; i < copies; i += 1) {
        const copy = set.cloneNode(true);
        copy.setAttribute("aria-hidden", "true");
        for (const link of copy.querySelectorAll("a")) link.tabIndex = -1;
        track.append(copy);
    }

    track.style.setProperty("--mk-badges-copies", String(copies));
    track.style.setProperty("--mk-badges-dur", `${(cycle / SPEED).toFixed(2)}s`);
};

let pending;
const rebuild = () => {
    clearTimeout(pending);
    pending = setTimeout(build, 150);
};

if (rail) {
    build();
    // Badges carry intrinsic dimensions, so decoding only shifts the measurement
    // when a file's attributes and its real aspect disagree.
    addEventListener("load", build);
    addEventListener("resize", rebuild);
    calm.addEventListener("change", build);
}
