// Decline confirm page: GET renders only; this button performs the decline.
(function () {
    const confirmBox = document.getElementById("decline-confirm");
    const btn = document.getElementById("decline-btn");
    if (!confirmBox || !btn) return;
    const errBox = document.getElementById("decline-error");
    const done = document.getElementById("decline-done");

    btn.addEventListener("click", async () => {
        btn.disabled = true;
        errBox.classList.add("hidden");
        try {
            const res = await fetch("/api/v1/invitations/decline", {
                method: "POST",
                headers: {
                    "Content-Type": "application/json",
                    "Accept": "application/json",
                    "X-Requested-With": "uptimepage",
                },
                body: JSON.stringify({ token: confirmBox.dataset.token }),
            });
            if (res.status === 204) {
                confirmBox.classList.add("hidden");
                done.classList.remove("hidden");
                return;
            }
            window.smRenderClientError(
                errBox,
                "Decline failed — the invitation may already be settled. Refresh to check.",
            );
            btn.disabled = false;
        } catch {
            window.smRenderClientError(errBox, "Network error — try again.");
            btn.disabled = false;
        }
    });
})();
