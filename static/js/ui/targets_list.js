(function () {
    function filterRowsByName(needle) {
        const q = needle.trim().toLowerCase();
        document.querySelectorAll("#target-rows tr[data-name]").forEach(row => {
            const name = row.dataset.name.toLowerCase();
            row.style.display = (q === "" || name.includes(q)) ? "" : "none";
        });
    }

    document.addEventListener("input", evt => {
        if (evt.target && evt.target.matches('input[name="q"][data-client-filter]')) {
            filterRowsByName(evt.target.value);
        }
    });
})();
