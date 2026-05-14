use std::collections::HashSet;
use std::sync::LazyLock;

/// Slugs an end-user cannot claim. Exact-match (case-insensitive — CITEXT
/// handles DB side; we lowercase before lookup). Auto-generated
/// `personal-{adj}-{noun}-{rand}` slugs pass because only the bare word
/// `personal` is in the list.
pub const RESERVED_SLUGS: &[&str] = &[
    // ── Generic / system ─────────────────────────────────────────────────
    "admin", "api", "app", "apps", "auth", "billing", "blog", "ci",
    "console", "dashboard", "dev", "docs", "embed", "feed", "feeds",
    "ftp", "git", "help", "home", "host", "imap", "internal", "login",
    "logout", "mail", "mx", "ns", "ns1", "ns2", "official", "page",
    "pages", "personal", "pop3", "portal", "private", "prod", "public",
    "register", "root", "rss", "secure", "settings", "signin", "signup",
    "site", "smtp", "ssh", "ssl", "staging", "static", "status",
    "support", "system", "team", "test", "tools", "tos", "user", "users",
    "vpn", "web", "webmail", "www",

    // ── Famous brands ────────────────────────────────────────────────────
    "apple", "google", "microsoft", "amazon", "meta", "facebook",
    "twitter", "x", "instagram", "tiktok", "youtube", "linkedin",
    "github", "gitlab", "bitbucket", "stripe", "paypal", "shopify",
    "slack", "discord", "zoom", "notion", "figma", "atlassian",
    "openai", "anthropic", "claude", "chatgpt", "perplexity",
    "nvidia", "amd", "intel", "tesla", "spacex",
    "spotify", "netflix", "disney", "uber", "airbnb", "doordash",
    "ibm", "oracle", "salesforce", "adobe", "samsung", "sony",
    "huawei", "xiaomi", "lenovo", "dell", "hp", "cisco",
    "twitch", "reddit", "pinterest", "snapchat", "whatsapp",
    "telegram", "signal", "wechat", "line", "kakaotalk",
    "ebay", "alibaba", "tencent", "baidu", "yandex",
    "vmware", "redhat", "ubuntu", "debian", "fedora",
    "docker", "kubernetes", "hashicorp", "terraform",
    "mongodb", "redis", "postgresql", "mysql", "snowflake",
    "databricks", "cloudflare", "akamai", "fastly", "vercel",
    "netlify", "heroku", "digitalocean", "linode",
    "twilio", "sendgrid", "mailchimp", "hubspot", "intercom",
    "zendesk", "freshworks", "asana", "trello", "monday",
    "linear", "jira", "confluence", "bitwarden", "1password",
    "lastpass", "okta", "auth0", "ping",

    // ── Competitors ──────────────────────────────────────────────────────
    "betterstack", "statuspage", "pingdom", "datadog", "uptimerobot",
    "pageruptime", "freshping", "cachet", "uptimekuma", "instatus",
    "checkly", "newrelic", "grafana", "honeycomb", "lightstep",
    "splunk", "elastic", "logz", "sumologic", "sentry",
    "bugsnag", "rollbar", "raygun", "appsignal",
];

static SET: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| RESERVED_SLUGS.iter().copied().collect());

/// O(1) exact-match check. Caller must pass a lowercase slug; validator does
/// that implicitly via charset rules.
pub fn is_reserved(slug: &str) -> bool {
    SET.contains(slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_reserved_words_match() {
        assert!(is_reserved("admin"));
        assert!(is_reserved("personal"));
        assert!(is_reserved("anthropic"));
    }

    #[test]
    fn non_reserved_words_pass() {
        assert!(!is_reserved("acme"));
        assert!(!is_reserved("personal-fox-3a9k7m"));
    }
}
