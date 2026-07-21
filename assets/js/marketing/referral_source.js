// Directory and launch-platform links arrive as `?ref=<source>` with the
// referrer stripped by rel="noreferrer", so Umami would file them under Direct.
const SOURCE = /^[a-z0-9][a-z0-9._-]{0,31}$/i;

window.tagReferralSource = (_type, payload) => {
  const url = new URL(payload.url, location.origin);
  const ref = url.searchParams.get("ref");

  if (ref && SOURCE.test(ref) && !url.searchParams.has("utm_source")) {
    url.searchParams.set("utm_source", ref);
    payload.url = url.pathname + url.search + url.hash;
  }

  return payload;
};
