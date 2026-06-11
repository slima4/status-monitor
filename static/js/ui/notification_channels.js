(function () {
    const form = document.getElementById("channel-form");
    if (!form) return;

    const isEdit = form.dataset.mode === "edit";
    const replaceCb = form.querySelector("[data-replace-config]");
    const configFs = form.querySelector("[data-config]");
    const kindFs = form.querySelector("[data-kind-chooser]");
    const nameInput = form.querySelector("[data-name-input]");
    const resultEl = form.querySelector("[data-test-result]");

    function currentKind() {
        return form.querySelector("input[name='kind']:checked")?.value || "slack";
    }

    const storedKind = currentKind();

    // True when the form's transport config is live: create, or edit with
    // "replace config" on. Gates BOTH what save submits and what the test
    // button exercises — false routes the test to the stored config.
    function usesFormConfig() {
        return !isEdit || !!(replaceCb && replaceCb.checked);
    }

    function syncTestHint() {
        const hint = form.querySelector("[data-test-hint]");
        if (!hint) return;
        hint.textContent = usesFormConfig() ? hint.dataset.hintForm : hint.dataset.hintStored;
    }

    function hideTestResult() {
        resultEl?.classList.add("hidden");
    }

    function showStatus(el, text, cls) {
        el.textContent = text;
        el.className = `font-mono text-xs ${cls}`;
        el.classList.remove("hidden");
    }

    // margin 4 = the QR spec's full quiet zone, needed against the dark
    // page for picky scanners.
    function renderQr(el, url) {
        if (typeof qrcode !== "function") throw new Error("QR library failed to load — refresh and try again");
        const qr = qrcode(0, "M");
        qr.addData(url);
        qr.make();
        el.innerHTML = qr.createSvgTag({ cellSize: 4, margin: 4 });
    }

    // First text node only — the name span may carry disabled/managed chips.
    function cardName(el) {
        return el.querySelector(".check-type-card__name")?.childNodes[0]?.textContent.trim() || "monitor";
    }

    // Make `channelId`'s presence in the monitor's alert bindings match
    // `bound`. GET-then-PATCH replaces the whole alerts array from a fresh
    // snapshot — last write wins, same as the monitor form's own save.
    async function setBinding(channelId, targetId, bound) {
        const headers = {
            "Accept": "application/json",
            "X-Requested-With": "uptimepage",
        };
        const res = await fetch(`/api/v1/targets/${targetId}`, { headers });
        if (!res.ok) throw new Error(`monitor fetch failed (${res.status})`);
        let alerts = (await res.json()).alerts || [];
        const present = alerts.some((b) => b.channel_id === channelId);
        if (bound === present) return;
        if (bound) alerts.push({ channel_id: channelId });
        else alerts = alerts.filter((b) => b.channel_id !== channelId);
        const patch = await fetch(`/api/v1/targets/${targetId}`, {
            method: "PATCH",
            headers: { ...headers, "Content-Type": "application/json" },
            body: JSON.stringify({ alerts }),
        });
        if (!patch.ok) {
            throw new Error(await window.smApiErrorMessage(patch, `update failed (${patch.status})`));
        }
    }

    function showVariant(kind) {
        form.querySelectorAll("[data-variant]").forEach(el => {
            el.classList.toggle("hidden", el.dataset.variant !== kind);
        });
        syncCentralTelegram(kind);
    }

    // One-tap create has no submittable config — the webhook creates the
    // channel — so the submit/test/bind affordances yield to connect.
    function syncCentralTelegram(kind) {
        const hide = kind === "telegram_app" && !isEdit;
        form.querySelector("button[type=submit]")?.classList.toggle("hidden", hide);
        const testRow = form.querySelector("[data-send-test]")?.parentElement;
        testRow?.classList.toggle("hidden", hide);
        if (hide) hideTestResult();
        form.querySelector("#used-by")?.classList.toggle("hidden", hide);
    }

    function syncNamePlaceholder() {
        const kind = currentKind();
        if (nameInput) nameInput.placeholder = `ops-${kind === "telegram_app" ? "telegram" : kind}`;
    }

    // On edit the secret is never shown; the config inputs stay disabled
    // until the operator opts to replace the whole transport config. The
    // type cards lock with them (one fieldset.disabled flip) — the kind is
    // part of the config, so a diverged card with no config submit would
    // lie about what gets saved. The revert below is cosmetic: the server
    // derives the stored kind from the config on PATCH regardless.
    function syncConfigEnabled() {
        if (!isEdit) return;
        const on = !!(replaceCb && replaceCb.checked);
        configFs.querySelectorAll("input, textarea, select").forEach(el => {
            if (el === replaceCb) return;
            el.disabled = !on;
        });
        if (kindFs) kindFs.disabled = !on;
        if (!on) {
            const stored = form.querySelector(`input[name='kind'][value='${storedKind}']`);
            if (stored) stored.checked = true;
            showVariant(storedKind);
            syncNamePlaceholder();
        }
        form.querySelector("[data-kind-locked-hint]")?.classList.toggle("hidden", on);
        syncTestHint();
    }

    function syncAll() {
        showVariant(currentKind());
        syncNamePlaceholder();
        syncConfigEnabled();
        syncTestHint();
    }

    syncAll();
    // Browsers restore form-control state on reload / history navigation
    // without firing change events — re-derive everything from it.
    window.addEventListener("pageshow", syncAll);

    form.addEventListener("change", (evt) => {
        if (evt.target.name === "kind") {
            showVariant(currentKind());
            syncNamePlaceholder();
            hideTestResult();
        }
    });
    // A green "✓ delivered" must not vouch for a config edited after the
    // test ran.
    form.addEventListener("input", (evt) => {
        if (evt.target.closest("[data-config]")) hideTestResult();
    });
    if (replaceCb) replaceCb.addEventListener("change", syncConfigEnabled);

    // "Test now": exercises the same notifier path a real incident uses.
    // Create / replace-config: POSTs the form's config without saving;
    // locked edit: tests the stored config by id.
    const testBtn = form.querySelector("[data-send-test]");
    if (testBtn) {
        const showResult = (text, cls) => showStatus(resultEl, text, cls);
        testBtn.addEventListener("click", async () => {
            clearErrors();
            let url = `${form.dataset.action}/test`;
            let body = null;
            if (usesFormConfig()) {
                const built = buildConfig();
                if (built.error) {
                    showResult(`✗ ${built.error}`, "flash-text flash-text--bad");
                    if (built.field) form.querySelector(`[name="${built.field}"]`)?.focus();
                    return;
                }
                // The prefilled "***" masks are not real secrets; the API
                // would reject them with PATCH-flavoured advice that the
                // test button can't follow.
                if (JSON.stringify(built.config).includes("***")) {
                    showResult(
                        isEdit
                            ? "✗ masked secrets (***) can't be tested — re-enter the real value, or untick \"Replace transport config\" to test the stored config"
                            : "✗ masked secrets (***) can't be tested — enter the real value",
                        "flash-text flash-text--bad",
                    );
                    return;
                }
                url = "/api/v1/notification-channels/test";
                body = JSON.stringify({ config: built.config });
            }
            testBtn.disabled = true;
            showResult("# sending test alert…", "text-quiet");
            try {
                const res = await fetch(url, {
                    method: "POST",
                    headers: {
                        "Content-Type": "application/json",
                        "Accept": "application/json",
                        "X-Requested-With": "uptimepage",
                    },
                    body,
                });
                if (res.ok) {
                    const enabledOn = !!form.querySelector("input[name='enabled']")?.checked;
                    showResult(
                        enabledOn
                            ? "✓ test alert delivered — check the destination"
                            : "✓ test alert delivered — note: with Enabled off, bound monitors won't alert through this channel",
                        "flash-text flash-text--ok font-medium",
                    );
                } else {
                    const msg = await window.smApiErrorMessage(res, `delivery failed (${res.status})`);
                    showResult(`✗ ${msg}`, "flash-text flash-text--bad font-medium");
                }
            } catch (err) {
                showResult(`✗ network error: ${err.message || err}`, "flash-text flash-text--bad font-medium");
            } finally {
                testBtn.disabled = false;
            }
        });
    }

    // Resend the verification mail for an unverified email channel (edit only).
    const resendBtn = form.querySelector("[data-resend-verification]");
    if (resendBtn) {
        const resendResult = form.querySelector("[data-resend-result]");
        const showResend = (text, cls) => showStatus(resendResult, text, cls);
        resendBtn.addEventListener("click", async () => {
            resendBtn.disabled = true;
            showResend("# sending…", "text-quiet");
            try {
                const res = await fetch(
                    `/api/v1/notification-channels/${resendBtn.dataset.channelId}/resend-verification`,
                    {
                        method: "POST",
                        headers: {
                            "Accept": "application/json",
                            "X-Requested-With": "uptimepage",
                        },
                    },
                );
                if (res.ok) {
                    showResend("✓ verification mail sent — check the inbox", "flash-text flash-text--ok font-medium");
                } else {
                    const msg = await window.smApiErrorMessage(res, `resend failed (${res.status})`);
                    showResend(`✗ ${msg}`, "flash-text flash-text--bad font-medium");
                }
            } catch (err) {
                showResend(`✗ network error: ${err.message || err}`, "flash-text flash-text--bad font-medium");
            } finally {
                resendBtn.disabled = false;
            }
        });
    }

    // Telegram setup helper: a t.me QR so the phone reaches the bot without
    // typing (the bot can't message anyone until they press Start), then a
    // chat-id probe over getUpdates. Both talk to the Bot API straight from
    // the browser with the token already sitting in the form.
    const tgQrBtn = form.querySelector("[data-tg-qr]");
    const tgDetectBtn = form.querySelector("[data-tg-detect]");
    if (tgQrBtn && tgDetectBtn) {
        const tgResult = form.querySelector("[data-tg-result]");
        const qrBox = form.querySelector("[data-tg-qr-box]");
        const qrImg = form.querySelector("[data-tg-qr-img]");
        const qrLink = form.querySelector("[data-tg-qr-link]");
        const tokenInput = form.querySelector("[name='telegram_bot_token']");
        const chatInput = form.querySelector("[name='telegram_chat_id']");
        const showTg = (text, cls) => showStatus(tgResult, text, cls);

        function tgToken() {
            const tok = (tokenInput?.value || "").trim();
            if (tok.includes("***")) {
                showTg("✗ the stored token is masked — tick \"Replace transport config\" and paste the real token first", "flash-text flash-text--bad");
                return null;
            }
            if (!/^\d+:[\w-]+$/.test(tok)) {
                showTg("✗ enter the bot token first (looks like 123456:ABC-DEF…)", "flash-text flash-text--bad");
                tokenInput?.focus();
                return null;
            }
            return tok;
        }

        function hideTgHelper() {
            tgResult?.classList.add("hidden");
            qrBox?.classList.add("hidden");
        }
        tokenInput?.addEventListener("input", hideTgHelper);
        form.addEventListener("change", (evt) => {
            if (evt.target.name === "kind") hideTgHelper();
        });

        async function tgApi(token, method) {
            const res = await fetch(`https://api.telegram.org/bot${token}/${method}`);
            const json = await res.json().catch(() => null);
            if (!json?.ok) {
                const desc = json?.description || `telegram api error (${res.status})`;
                if (/webhook/i.test(desc)) {
                    throw new Error("this bot has a webhook configured, which blocks the chat-id probe — use @userinfobot for the id, or delete the webhook if the bot is dedicated to alerts");
                }
                throw new Error(desc);
            }
            return json.result;
        }

        tgQrBtn.addEventListener("click", async () => {
            const token = tgToken();
            if (!token) return;
            tgQrBtn.disabled = true;
            showTg("# asking the bot api…", "text-quiet");
            try {
                const me = await tgApi(token, "getMe");
                if (!me.username) throw new Error("the bot has no username");
                const url = `https://t.me/${me.username}`;
                renderQr(qrImg, url);
                qrLink.textContent = url;
                qrLink.href = url;
                qrBox.classList.remove("hidden");
                showTg(`✓ @${me.username} — scan, press Start, then detect the chat ID`, "flash-text flash-text--ok font-medium");
            } catch (err) {
                showTg(`✗ ${err.message || err}`, "flash-text flash-text--bad font-medium");
            } finally {
                tgQrBtn.disabled = false;
            }
        });

        tgDetectBtn.addEventListener("click", async () => {
            const token = tgToken();
            if (!token) return;
            tgDetectBtn.disabled = true;
            showTg("# checking the bot's recent messages…", "text-quiet");
            try {
                // offset=-100 biases the window toward the newest pending
                // updates on busy bots.
                const updates = await tgApi(token, "getUpdates?offset=-100");
                const chats = new Map();
                for (const u of updates) {
                    // A my_chat_member update only counts when the bot was
                    // added — a kick/leave must not nominate that chat.
                    const joined = ["member", "administrator", "creator"]
                        .includes(u.my_chat_member?.new_chat_member?.status);
                    const c = u.message?.chat || u.channel_post?.chat
                        || (joined ? u.my_chat_member?.chat : null);
                    if (!c) continue;
                    // Re-insert so Map order is "last seen", not "first seen".
                    chats.delete(c.id);
                    chats.set(c.id, c);
                }
                if (!chats.size) {
                    throw new Error("no messages yet — press Start in the bot chat (or write anything in the group), then try again");
                }
                const chat = [...chats.values()].pop();
                chatInput.value = String(chat.id);
                chatInput.dispatchEvent(new Event("input", { bubbles: true }));
                const who = chat.title || (chat.username && `@${chat.username}`) || [chat.first_name, chat.last_name].filter(Boolean).join(" ");
                showTg(`✓ chat ID ${chat.id}${who ? ` (${who})` : ""} filled in`, "flash-text flash-text--ok font-medium");
            } catch (err) {
                showTg(`✗ ${err.message || err}`, "flash-text flash-text--bad font-medium");
            } finally {
                tgDetectBtn.disabled = false;
            }
        });
    }

    // One-tap Telegram: mint a code, show t.me link + QR, poll until the
    // chat presses Start, then jump to the channel the webhook created.
    const tgaBtn = form.querySelector("[data-tga-connect]");
    if (tgaBtn) {
        const tgaStatus = form.querySelector("[data-tga-status]");
        const tgaQrBox = form.querySelector("[data-tga-qr-box]");
        const tgaQrImg = form.querySelector("[data-tga-qr-img]");
        const tgaLink = form.querySelector("[data-tga-link]");
        const tgaGroupCb = form.querySelector("[data-tga-group]");
        const showTga = (text, cls) => showStatus(tgaStatus, text, cls);
        const headers = { "Accept": "application/json", "X-Requested-With": "uptimepage" };
        let pollTimer = null;
        let mintedLinks = null;

        function resetTga() {
            if (pollTimer) clearInterval(pollTimer);
            pollTimer = null;
            mintedLinks = null;
            tgaQrBox.classList.add("hidden");
            tgaStatus.classList.add("hidden");
            tgaBtn.disabled = false;
        }

        // One code serves both destinations; the toggle only swaps which
        // deep link (start vs startgroup) is shown.
        function renderTgaDest() {
            if (!mintedLinks) return;
            const url = tgaGroupCb?.checked ? mintedLinks.group_deep_link : mintedLinks.deep_link;
            tgaLink.textContent = url;
            tgaLink.href = url;
            // The t.me link works without the QR — degrade, don't dead-end.
            try {
                renderQr(tgaQrImg, url);
                tgaQrImg.classList.remove("hidden");
            } catch {
                tgaQrImg.classList.add("hidden");
            }
            tgaQrBox.classList.remove("hidden");
            showTga(
                tgaGroupCb?.checked
                    ? "# waiting — open the link (or scan), pick the group, and confirm…"
                    : "# waiting — open the link (or scan) and press Start…",
                "text-quiet",
            );
        }
        tgaGroupCb?.addEventListener("change", renderTgaDest);
        form.addEventListener("change", (evt) => {
            if (evt.target.name === "kind") resetTga();
        });
        // bfcache restore: the poll handle is gone, the code may be stale.
        window.addEventListener("pageshow", (evt) => {
            if (evt.persisted) resetTga();
        });

        function pollLink(id) {
            pollTimer = setInterval(async () => {
                let body;
                try {
                    const res = await fetch(`/api/v1/notification-channels/telegram-link/${id}`, { headers });
                    if (!res.ok) return; // transient; expiry resolves it
                    body = await res.json();
                } catch { return; }
                if (body.status === "consumed" && body.channel_id) {
                    clearInterval(pollTimer);
                    pollTimer = null;
                    showTga("✓ linked — opening the new channel…", "flash-text flash-text--ok font-medium");
                    window.location = `/settings/notifications/${body.channel_id}/edit`;
                } else if (body.status !== "pending") {
                    clearInterval(pollTimer);
                    pollTimer = null;
                    tgaQrBox.classList.add("hidden");
                    tgaBtn.disabled = false;
                    showTga("✗ the link expired before it was used — connect again for a fresh one", "flash-text flash-text--bad font-medium");
                }
            }, 2000);
        }

        tgaBtn.addEventListener("click", async () => {
            clearErrors();
            resetTga();
            tgaBtn.disabled = true;
            showTga("# creating a single-use link…", "text-quiet");
            const name = (nameInput?.value || "").trim();
            let body;
            try {
                const res = await fetch("/api/v1/notification-channels/telegram-link", {
                    method: "POST",
                    headers: { ...headers, "Content-Type": "application/json" },
                    body: JSON.stringify(name ? { name } : {}),
                });
                if (!res.ok) {
                    const msg = await window.smApiErrorMessage(res, `link failed (${res.status})`);
                    showTga(`✗ ${msg}`, "flash-text flash-text--bad font-medium");
                    tgaBtn.disabled = false;
                    return;
                }
                body = await res.json();
            } catch (err) {
                showTga(`✗ network error: ${err.message || err}`, "flash-text flash-text--bad font-medium");
                tgaBtn.disabled = false;
                return;
            }
            mintedLinks = body;
            renderTgaDest();
            pollLink(body.id);
        });
    }

    // "Add to Slack": the button is a full-page OAuth redirect; the QR
    // variant fetches the same single-use authorize URL for a phone that is
    // signed in here. Callback bounces land back with ?slack=<outcome>.
    const slackAdd = form.querySelector("[data-slack-add]");
    if (slackAdd) {
        const slackNote = form.querySelector("[data-slack-note]");
        const slackQrBox = form.querySelector("[data-slack-qr-box]");
        const slackQrImg = form.querySelector("[data-slack-qr-img]");
        const slackQrBtn = form.querySelector("[data-slack-qr]");
        const showSlack = (text, cls) => showStatus(slackNote, text, cls);
        const outcome = new URLSearchParams(window.location.search).get("slack");
        if (outcome === "cancelled") {
            showSlack("# slack connect cancelled — nothing was created", "text-quiet");
        } else if (outcome === "quota") {
            showSlack("✗ notification-channel limit reached for this plan", "flash-text flash-text--bad font-medium");
        } else if (outcome === "failed") {
            showSlack("✗ slack connect failed — try again, or paste a webhook URL below", "flash-text flash-text--bad font-medium");
        }
        slackQrBtn?.addEventListener("click", async () => {
            slackQrBtn.disabled = true;
            try {
                const res = await fetch("/auth/slack/start?format=json", {
                    headers: { "Accept": "application/json", "X-Requested-With": "uptimepage" },
                });
                if (!res.ok) throw new Error(`slack start failed (${res.status})`);
                const body = await res.json();
                renderQr(slackQrImg, body.url);
                slackQrBox.classList.remove("hidden");
                showSlack("# scan with a phone that's signed in here and finish on Slack — single-use, expires in 10 minutes", "text-quiet");
            } catch (err) {
                showSlack(`✗ ${err.message || err}`, "flash-text flash-text--bad font-medium");
            } finally {
                slackQrBtn.disabled = false;
            }
        });
    }

    const submitBtn = form.querySelector("button[type=submit]");
    form.addEventListener("submit", async (evt) => {
        evt.preventDefault();
        if (submitBtn.disabled) return;
        clearErrors();
        const built = buildBody();
        if (built.error) {
            renderClientError(built.error);
            if (built.field) form.querySelector(`[name="${built.field}"]`)?.focus();
            return;
        }

        const label = submitBtn.textContent;
        submitBtn.disabled = true;
        submitBtn.textContent = "saving…";
        let navigating = false;
        try {
            let res;
            try {
                res = await fetch(form.dataset.action, {
                    method: form.dataset.method,
                    headers: {
                        "Content-Type": "application/json",
                        "Accept": "application/json",
                        "X-Requested-With": "uptimepage",
                    },
                    body: JSON.stringify(built.payload),
                });
            } catch (err) {
                renderClientError(`Network error: ${err.message || err}`);
                return;
            }
            if (res.ok) {
                if (!isEdit) {
                    const bound = await bindSelected(res);
                    if (bound.failed.length) {
                        // The channel exists now — a re-submit would dead-end
                        // on CHANNEL_NAME_TAKEN (or duplicate the channel
                        // under a tweaked name) and re-bind the already-bound
                        // picks. The edit page shows the true state.
                        if (bound.channelId) {
                            navigating = true;
                            window.location = `/settings/notifications/${bound.channelId}/edit`;
                            return;
                        }
                        renderClientError(
                            `Channel created, but binding failed for ${bound.failed.join(", ")} — open the channel from the list to finish.`,
                        );
                        return;
                    }
                }
                navigating = true;
                window.location = "/settings/notifications";
                return;
            }
            let body;
            try { body = await res.json(); }
            catch { renderClientError(`Request failed (${res.status})`); return; }
            renderApiError(body, res.status);
        } finally {
            if (!navigating) { submitBtn.disabled = false; submitBtn.textContent = label; }
        }
    });

    // Create-mode "bind to monitors": bind each picked monitor to the
    // channel just created. Returns the new channel id and descriptions of
    // the bindings that failed (empty = all good).
    async function bindSelected(res) {
        const picked = [...form.querySelectorAll("[data-used-by-grid] [data-bound-card]")];
        if (!picked.length) return { channelId: null, failed: [] };
        let channelId = null;
        try { channelId = (await res.json())?.id; } catch { /* handled below */ }
        if (!channelId) {
            return { channelId, failed: ["every monitor (the create response carried no channel id)"] };
        }
        const failed = [];
        for (const [i, card] of picked.entries()) {
            if (picked.length > 3) submitBtn.textContent = `binding ${i + 1}/${picked.length}…`;
            try {
                await setBinding(channelId, card.dataset.targetId, true);
            } catch (err) {
                failed.push(`${cardName(card)} (${err.message || err})`);
            }
        }
        return { channelId, failed };
    }

    function buildBody() {
        const data = new FormData(form);
        const name = (data.get("name") || "").trim();
        if (!name) return { error: "Name is required.", field: "name" };
        const payload = {
            name,
            enabled: data.get("enabled") === "on",
        };

        // Edit + "replace config" unchecked: omit config so the stored
        // secret is preserved (the API rejects a re-submitted "***").
        if (usesFormConfig()) {
            const built = buildConfig();
            if (built.error) return built;
            payload.config = built.config;
        }
        return { payload };
    }

    // The transport config exactly as the API expects it, from the current
    // form values. Shared by save and "test now".
    function buildConfig() {
        const data = new FormData(form);
        const kind = currentKind();
        if (kind === "slack" || kind === "discord" || kind === "msteams" || kind === "google_chat") {
            return {
                config: {
                    type: kind,
                    webhook_url: (data.get(`${kind}_webhook_url`) || "").trim(),
                },
            };
        }
        if (kind === "webhook") {
            let headers;
            try {
                const raw = (data.get("webhook_headers") || "").trim();
                headers = raw ? JSON.parse(raw) : {};
                if (typeof headers !== "object" || headers === null || Array.isArray(headers)) {
                    throw new Error("not an object");
                }
            } catch {
                return { error: "Headers must be a JSON object — e.g. {\"Authorization\": \"Bearer …\"}", field: "webhook_headers" };
            }
            const config = {
                type: "webhook",
                url: (data.get("webhook_url") || "").trim(),
                headers,
            };
            const secret = (data.get("webhook_secret") || "").trim();
            if (secret) {
                config.secret = secret;
            }
            return { config };
        }
        if (kind === "whatsapp") {
            const template = (data.get("whatsapp_template_name") || "").trim();
            if (!template) {
                return {
                    error: "Template name is required — WhatsApp silently drops template-less alerts outside the 24-hour service window.",
                    field: "whatsapp_template_name",
                };
            }
            const config = {
                type: "whatsapp",
                access_token: (data.get("whatsapp_access_token") || "").trim(),
                phone_number_id: (data.get("whatsapp_phone_number_id") || "").trim(),
                to: (data.get("whatsapp_to") || "").trim(),
                template_name: template,
            };
            const language = (data.get("whatsapp_language_code") || "").trim();
            if (language) config.language_code = language;
            return { config };
        }
        if (kind === "email") {
            return {
                config: {
                    type: "email",
                    to: (data.get("email_to") || "").trim().toLowerCase(),
                },
            };
        }
        if (kind === "telegram_app") {
            // The API rejects this kind in request bodies.
            return { error: "Linked telegram channels have no config to submit — use \"connect telegram\", or untick \"Replace transport config\" to keep the stored link." };
        }
        return {
            config: {
                type: "telegram",
                bot_token: (data.get("telegram_bot_token") || "").trim(),
                chat_id: (data.get("telegram_chat_id") || "").trim(),
            },
        };
    }

    // Monitor binding picker, both modes. Edit: each pick/unpick PATCHes
    // immediately. Create: picks are local until the submit handler binds
    // them. Cards move between the grids in place — no reload, so unsaved
    // form edits survive.
    const bindRoot = form.querySelector("[data-bind-root]");
    if (bindRoot) {
        const channelId = bindRoot.dataset.channelId; // empty on create
        const addBtn = bindRoot.querySelector("[data-add-monitor]");
        const picker = bindRoot.querySelector("#bind-picker");
        const bindResult = bindRoot.querySelector("[data-bind-result]");
        const usedByGrid = bindRoot.querySelector("[data-used-by-grid]");
        const bindGrid = bindRoot.querySelector("[data-bind-grid]");
        const searchInput = bindRoot.querySelector("[data-picker-search]");
        const showDisabledCb = bindRoot.querySelector("[data-picker-show-disabled]");
        const pagerEl = bindRoot.querySelector("[data-picker-pager]");
        const pagerInfo = bindRoot.querySelector("[data-pager-info]");
        const pagerPrev = bindRoot.querySelector("[data-pager-prev]");
        const pagerNext = bindRoot.querySelector("[data-pager-next]");
        const PAGE_SIZE = 20;
        let page = 0;
        const showBindResult = (text, cls) => showStatus(bindResult, text, cls);
        const setPickerOpen = (open) => {
            picker.classList.toggle("hidden", !open);
            addBtn.setAttribute("aria-expanded", String(open));
        };
        addBtn.addEventListener("click", () => {
            setPickerOpen(picker.classList.contains("hidden"));
        });

        // One derive-everything-from-DOM render: counts, empty-state notes,
        // and the search/show-disabled/pagination window.
        function applyPicker() {
            const bound = usedByGrid.querySelectorAll("[data-bound-card]").length;
            const all = [...bindGrid.querySelectorAll("[data-bind-monitor]")];
            const avail = all.length;

            const someEl = document.querySelector("[data-bound-some]");
            if (someEl) {
                someEl.classList.toggle("hidden", bound === 0);
                document.querySelector("[data-bound-none]")?.classList.toggle("hidden", bound > 0);
                const count = someEl.querySelector("[data-bound-count]");
                if (count) count.textContent = `${bound} monitor${bound === 1 ? "" : "s"}`;
            }
            bindRoot.querySelector("[data-used-by-note]")?.classList.toggle("hidden", bound > 0);
            bindRoot.querySelector("[data-picker-none]")?.classList.toggle("hidden", bound + avail > 0);
            bindRoot.querySelector("[data-picker-allbound]")?.classList.toggle("hidden", avail > 0 || bound === 0);
            bindRoot.querySelector("[data-picker-desc]")?.classList.toggle("hidden", avail === 0);
            bindRoot.querySelector("[data-picker-controls]")?.classList.toggle("hidden", avail === 0);
            bindGrid.classList.toggle("hidden", avail === 0);

            const q = (searchInput?.value || "").trim().toLowerCase();
            const showDisabled = !!showDisabledCb?.checked;
            const matched = all.filter((b) =>
                (showDisabled || b.dataset.enabled !== "false")
                && (!q || `${b.textContent} ${b.dataset.tags || ""}`.toLowerCase().includes(q)));
            const pages = Math.max(1, Math.ceil(matched.length / PAGE_SIZE));
            page = Math.min(page, pages - 1);
            const visible = new Set(matched.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE));
            all.forEach((b) => b.classList.toggle("hidden", !visible.has(b)));
            pagerEl.classList.toggle("hidden", matched.length <= PAGE_SIZE);
            pagerInfo.textContent = `page ${page + 1}/${pages} · ${matched.length} match${matched.length === 1 ? "" : "es"}`;
            pagerPrev.disabled = page === 0;
            pagerNext.disabled = page >= pages - 1;
            bindRoot.querySelector("[data-picker-nomatch]")?.classList
                .toggle("hidden", matched.length > 0 || avail === 0);
        }

        // Enter in the picker search must not submit the form.
        searchInput?.addEventListener("keydown", (evt) => {
            if (evt.key === "Enter") evt.preventDefault();
        });
        // `/` jumps to the picker search (opening the picker if needed) —
        // same hotkey as the monitors list toolbar.
        document.addEventListener("keydown", (evt) => {
            if (evt.key !== "/" || evt.metaKey || evt.ctrlKey || evt.altKey) return;
            if (document.querySelector("dialog[open]")) return;
            const t = evt.target;
            if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
            if (!searchInput) return;
            evt.preventDefault();
            setPickerOpen(true);
            searchInput.focus();
            searchInput.select();
        });
        searchInput?.addEventListener("input", () => { page = 0; applyPicker(); });
        showDisabledCb?.addEventListener("change", () => { page = 0; applyPicker(); });
        pagerPrev?.addEventListener("click", () => { page -= 1; applyPicker(); });
        pagerNext?.addEventListener("click", () => { page += 1; applyPicker(); });
        applyPicker();

        function moveToUsedBy(btn) {
            const id = btn.dataset.targetId;
            const wrap = document.createElement("div");
            wrap.className = "relative min-w-0";
            wrap.dataset.boundCard = "";
            wrap.dataset.targetId = id;
            wrap.dataset.enabled = btn.dataset.enabled;
            wrap.dataset.tags = btn.dataset.tags || "";
            // On create the card must not navigate away mid-form; once the
            // binding is live (edit) it links to the monitor.
            let card;
            if (channelId) {
                card = document.createElement("a");
                card.href = `/targets/${id}/edit`;
            } else {
                card = document.createElement("div");
            }
            card.className = btn.className;
            card.classList.remove("text-left", "hidden");
            card.classList.add("h-full");
            card.innerHTML = btn.innerHTML;
            const unbind = document.createElement("button");
            unbind.type = "button";
            unbind.dataset.unbindMonitor = "";
            unbind.dataset.targetId = id;
            unbind.setAttribute("aria-label", `unbind ${cardName(btn)}`);
            unbind.title = channelId
                ? "unbind — stop alerting through this channel"
                : "remove — won't be bound when the channel is created";
            unbind.className = "unbind-btn";
            unbind.textContent = "×";
            wrap.append(card, unbind);
            usedByGrid.insertBefore(wrap, addBtn);
            btn.remove();
        }

        function moveToPicker(wrap) {
            const card = wrap.querySelector(".check-type-card");
            const btn = document.createElement("button");
            btn.type = "button";
            btn.dataset.bindMonitor = "";
            btn.dataset.targetId = wrap.dataset.targetId;
            btn.dataset.enabled = wrap.dataset.enabled;
            btn.dataset.tags = wrap.dataset.tags || "";
            btn.className = card.className;
            btn.classList.remove("h-full");
            btn.classList.add("text-left");
            btn.innerHTML = card.innerHTML;
            const name = cardName(btn).toLowerCase();
            const after = [...bindGrid.querySelectorAll("[data-bind-monitor]")]
                .find((b) => cardName(b).toLowerCase() > name);
            bindGrid.insertBefore(btn, after || null);
            wrap.remove();
        }

        bindRoot.addEventListener("click", async (evt) => {
            const bindBtn = evt.target.closest("[data-bind-monitor]");
            const unbindBtn = evt.target.closest("[data-unbind-monitor]");
            if (!bindBtn && !unbindBtn) return;
            const name = cardName(bindBtn || unbindBtn.closest("[data-bound-card]"));

            // Create mode: local selection only — the submit handler binds.
            if (!channelId) {
                if (bindBtn) moveToUsedBy(bindBtn);
                else moveToPicker(unbindBtn.closest("[data-bound-card]"));
                bindResult.classList.add("hidden");
                applyPicker();
                return;
            }

            if (unbindBtn) {
                let body = `${name} stops alerting through this channel. You can re-add it any time.`;
                try {
                    const r = await fetch(`/api/v1/targets/${unbindBtn.dataset.targetId}`, {
                        headers: { "Accept": "application/json", "X-Requested-With": "uptimepage" },
                    });
                    if (r.ok && ((await r.json()).alerts || []).length === 1) {
                        body = `This is the only channel ${name} alerts through — unbinding silences it entirely. You can re-add it any time.`;
                    }
                } catch { /* default copy */ }
                const ok = await window.smConfirm({
                    title: "Unbind monitor?",
                    body,
                    confirmLabel: "unbind",
                    danger: true,
                });
                if (!ok) return;
            }
            bindRoot.querySelectorAll("[data-bind-monitor], [data-unbind-monitor]")
                .forEach((b) => { b.disabled = true; });
            showBindResult(bindBtn ? "# binding…" : "# unbinding…", "text-quiet");
            try {
                if (bindBtn) {
                    await setBinding(channelId, bindBtn.dataset.targetId, true);
                    moveToUsedBy(bindBtn);
                    showBindResult(`✓ ${name} now alerts through this channel`, "flash-text flash-text--ok font-medium");
                } else {
                    await setBinding(channelId, unbindBtn.dataset.targetId, false);
                    moveToPicker(unbindBtn.closest("[data-bound-card]"));
                    showBindResult(`✓ ${name} unbound — it no longer alerts through this channel`, "flash-text flash-text--ok font-medium");
                }
                applyPicker();
            } catch (err) {
                // Surface the failure even if the picker was collapsed
                // while the request was in flight.
                setPickerOpen(true);
                showBindResult(`✗ ${err.message || err}`, "flash-text flash-text--bad font-medium");
            } finally {
                bindRoot.querySelectorAll("[data-bind-monitor], [data-unbind-monitor]")
                    .forEach((b) => { b.disabled = false; });
            }
        });
    }

    function clearErrors() {
        window.smClearFormErrors(document.getElementById("form-errors"));
    }

    function renderClientError(msg) {
        window.smRenderClientError(document.getElementById("form-errors"), msg);
    }

    function renderApiError(json, status) {
        window.smRenderApiError(document.getElementById("form-errors"), json, status);
    }
})();
