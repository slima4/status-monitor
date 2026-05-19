// htmx 2 JSON request encoder.
//
// htmx 2 hands `encodeParameters` a values Proxy whose `toJSON` already maps
// the form fields to a plain object, so the body is a one-line
// `JSON.stringify` — no manual FormData walk, no `getExpressionVars`, no
// `overrideMimeType` (all htmx-1-era shims). Duplicate field names are not
// used by any form that opts into this extension.
(function () {
  htmx.defineExtension('json-enc', {
    onEvent: function (name, evt) {
      if (name === 'htmx:configRequest') {
        evt.detail.headers['Content-Type'] = 'application/json';
      }
    },
    encodeParameters: function (_xhr, parameters) {
      return JSON.stringify(parameters);
    },
  });
})();
