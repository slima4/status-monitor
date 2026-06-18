+++
title = "The night Emma stopped opening dashboards"
date = "2026-06-18"
slug = "ask-dont-click"
excerpt = "An alert at 2 a.m., and an on-call engineer who never opened a single tab. A short story about asking your monitoring questions instead of clicking through it — and why that changes the worst hour of the night."
tags = ["mcp", "ai", "monitoring", "on-call"]
draft = false
+++

# The night Emma stopped opening dashboards

The alert woke Emma at 2 a.m. On-call again.

The old reflex started: reach for the laptop, open the dashboard, find
the right view, filter, squint. Six steps before you even know what's
wrong. Instead, half-awake, she just asked the assistant already open on
her screen: *"What's down right now, and since when?"*

The answer arrived in seconds, in plain words. Checkout had been slow for
fifteen minutes. No tabs. No filters.

*"Why is checkout slow?"* The assistant didn't guess — it read the real
timing and told her: the **DNS** step. Not the server, not the database.
A whole branch of the search tree, pruned in one sentence, without her
opening a single log.

*"Show me the open incidents and the timeline."* Everything she'd
normally hop between four screens to assemble — pulled into the same chat
window, in order.

Then she acted, still without leaving the conversation. *"Pause the
staging monitor."* A confirmation prompt appeared — naming the exact
monitor and effect — she approved, done. She ran a fresh check on the API
with one line. She acknowledged the payments incident with a note for the
morning crew. Three actions, zero menus.

By 2:09 she was back in bed.

## What actually changed

Emma didn't get smarter overnight. The *interface* did.

For years, working with monitoring meant the same loop: open the
dashboard, navigate, read, interpret, act. Five steps, every one a place
to lose time and focus — especially at 2 a.m. with one eye open.

What Emma used is **MCP** — the Model Context Protocol — the standard
that lets an AI assistant actually *call* your tools instead of guessing
about them. Her monitoring exposes itself as a set of tools; her chat
client discovers them and reads back real, typed data. "What's broken?"
stops being something you go and look up, and becomes something you ask.

The quiet part that makes it safe to use half-asleep: reading is free,
but **every action waited for her approval.** The assistant could pause a
monitor only after showing her exactly what it was about to do. Speed
without handing over the keys.

That's the whole shift. The dashboard didn't go away — she just didn't
need it at the worst hour of the night. The questions came to her, the
answers came back in plain language, and the fix happened from the same
window she was already looking at.

Calm, confident, back asleep in nine minutes.

Point your own assistant at it: [how the MCP server
works](/mcp-server), and [the security thinking](/blog/mcp-server) behind
letting an AI near production.
