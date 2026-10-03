// framecc.nomadsgalaxy.com: the landing page, the install script, and /dl/<file> forwarded to
// the latest GitHub release, so the one-line install never names GitHub. Deploying puts
// index.html and ../install in the two constants below (site/README.md).
const PAGE = "__PAGE__";
const INSTALL = "__INSTALL__";
const RELEASE = "https://github.com/nomadsgalaxy/Command-Center/releases/latest/download/";

export default {
  async fetch(req) {
    const url = new URL(req.url);
    if (url.pathname === "/install" || url.pathname === "/install.sh") {
      return new Response(INSTALL, { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "max-age=300" } });
    }
    if (url.pathname.startsWith("/dl/")) {
      const file = url.pathname.slice(4);
      // only plain file names: cc-install-<arch>, cc-host-<arch>, SHA256SUMS…
      if (!/^[A-Za-z0-9._-]+$/.test(file)) return new Response("not found\n", { status: 404 });
      return Response.redirect(RELEASE + file, 302);
    }
    if (url.pathname === "/" || url.pathname === "/index.html") {
      return new Response(PAGE, { headers: { "content-type": "text/html; charset=utf-8", "cache-control": "max-age=300" } });
    }
    return Response.redirect(url.origin + "/", 302);
  },
};
