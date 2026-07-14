# Uptimepage Bot

**Last updated:** 2026-07-14

If you found this page in your server logs, our bot requested a page on your site. This page explains what it is, why it happened, and how to stop it.

## What It Is

Uptimepage is an uptime monitoring service. Someone using our service configured a check against a URL on your site. Every few seconds or minutes, we request that URL and record whether it responded, how fast, and with what status code. That is the whole job.

Every request we make was configured by a person. We do not discover sites on our own, and we do not crawl.

## How to Identify It

Our probes send a `User-Agent` that starts with `uptimepage/` and links back to this page:

```
uptimepage/1.0.0 (+https://uptimepage.dev/bot)
```

The version number changes as we release. The `uptimepage/` prefix and the link do not.

## What It Does and Does Not Do

We request the exact URL that was configured. Nothing else.

- We follow redirects, up to 10 hops.
- We request one URL per check. We do not follow links on your pages.
- We do not crawl or spider your site.
- We do not scan ports.
- We do not probe for admin panels, config files, or vulnerabilities.

A check can run as often as every 10 seconds. Most run every 60 seconds.

## robots.txt

We do not read your `robots.txt`, and we want to be straightforward about why.

`robots.txt` is a protocol for crawlers, which discover and index pages on their own. We are not a crawler. We fetch a single URL that a person explicitly asked us to watch, and monitoring is usually the thing that person most wants to keep working. Blocking it in `robots.txt` would silently break their monitoring rather than protect you.

If you do not want us requesting a URL on your site, the ways to stop it are below and they work regardless of `robots.txt`.

## Where the Requests Come From

We probe from three regions:

| Region | Location |
| --- | --- |
| `eu-helsinki` | Helsinki, Finland |
| `apac-sg` | Singapore |
| `us-east` | Secaucus, United States |

Only `eu-helsinki` currently has a stable outbound IP address:

```
204.168.246.94
```

Our Singapore and United States probes run on Fly.io, whose outbound addresses are not static and can change without notice. **Do not build an IP allowlist from them.** If you allowlist only the address above, checks from the other two regions will still be blocked.

If you need a full static IP allowlist for all three regions, email us and we will set one up.

## How to Allowlist Us

Allowlist by `User-Agent`. Match requests whose `User-Agent` contains `uptimepage/`, and let them through your WAF, rate limiter, or bot protection.

This is worth doing. If your WAF challenges or blocks our probes, the person monitoring your site sees failures that look like an outage on your side when nothing is actually wrong.

## If You Did Not Authorise This

Anyone can type a URL into our service, including a URL on a site they do not own. If you did not authorise monitoring of your site and you want it to stop, tell us and we will block it.

Email **hello@uptimepage.dev** with the subject `[ABUSE]` and include:

1. The URL or domain being requested
2. That you are the owner or operator of it

We will block the target so it cannot be monitored through our service. You do not need to prove ownership for us to act on a straightforward request to stop probing a domain.

Unauthorised monitoring is a violation of our [Abuse Policy](/abuse-policy).

## Reporting a Problem

If our bot is behaving badly, requesting too often, ignoring what is written on this page, or causing you load, email **hello@uptimepage.dev** and we will look into it.

## Contact

hello@uptimepage.dev
