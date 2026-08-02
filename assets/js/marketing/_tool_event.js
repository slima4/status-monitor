// One event per view per mode: dragging a slider is one act of use, not fifty.
// Properties are fixed choices and counts, never anything a visitor typed.
const fired = new Set();

function once(name, tool, props) {
    const key = `${name}:${tool}:${props.mode ?? ""}`;
    if (fired.has(key)) return;
    fired.add(key);
    window.umami?.track(name, { tool, ...props });
}

export function toolUsed(tool, props = {}) {
    once("tool-used", tool, props);
}

// What people expected to work and didn't.
export function toolError(tool, props = {}) {
    once("tool-error", tool, props);
}
