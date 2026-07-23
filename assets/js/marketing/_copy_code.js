// Adds a copy button to every code block inside `root`. Shared by the docs
// and blog pages. Progressive enhancement: without it the code stays
// selectable, so no reader loses access.
export function addCopyButtons(root) {
    for (const pre of root.querySelectorAll("pre")) {
        if (pre.dataset.copy) continue;
        pre.dataset.copy = "1";
        const button = document.createElement("button");
        button.type = "button";
        button.className = "mk-code-copy";
        button.textContent = "copy";
        button.addEventListener("click", async () => {
            const code = pre.querySelector("code");
            try {
                await navigator.clipboard.writeText((code || pre).innerText);
                button.textContent = "copied";
            } catch {
                button.textContent = "press ctrl+c";
            }
            setTimeout(() => {
                button.textContent = "copy";
            }, 1600);
        });
        pre.appendChild(button);
    }
}
