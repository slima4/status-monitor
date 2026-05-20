+++
title = "Why uptime monitoring should be boring"
date = "2026-05-20"
slug = "boring-uptime"
excerpt = "Good monitoring doesn’t need clever architecture. It needs a tight feedback loop, honest numbers, and the discipline to not over-build."
tags = ["uptime", "ops", "rust"]
draft = false
+++

# Why uptime monitoring should be boring

A monitor that surprises you is doing the wrong job. The interesting parts
of your stack belong in your product, not your watchdog.

## Three things a monitor must get right

1. **Honest signal.** A check that flakes for unrelated reasons is worse
   than no check at all — alert fatigue compounds.
2. **Cheap to run.** If the watchdog is expensive, it gets switched off
   under cost pressure exactly when you need it most.
3. **Cacheable surfaces.** Public status pages get hit by panicked humans
   and panicked bots in equal measure. Serve a flat byte stream.

## Boring is hard

The hard part isn’t async, it isn’t ClickHouse, it isn’t the templating
layer. The hard part is *not* adding features that make the monitor itself
fragile. Every quarter, the temptation is to wire in one more data source,
one more dashboard. Every quarter, the answer should usually be no.

## What we actually do

- One Rust binary. One Postgres. One ClickHouse. One container per piece.
- Per-minute counters roll up to per-day buckets in a materialised view —
  the read path never scans raw rows.
- The public status page renders to bytes, lives in an in-memory cache,
  and ships with `Cache-Control: public, max-age=10`. The CDN does the
  rest.

If your monitor is more exciting than your product, the org chart is
wrong.
