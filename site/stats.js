// Cloudflare Web Analytics: counts page views without cookies and without
// identifying visitors. Paste the token from the Cloudflare dashboard below.
(function () {
  var TOKEN = ""; // Web Analytics → your site → "JS snippet" token
  if (!TOKEN || /^(localhost|127\.0\.0\.1)$/.test(location.hostname)) return;
  var s = document.createElement("script");
  s.defer = true;
  s.src = "https://static.cloudflareinsights.com/beacon.min.js";
  s.setAttribute("data-cf-beacon", JSON.stringify({ token: TOKEN }));
  document.head.appendChild(s);
})();
