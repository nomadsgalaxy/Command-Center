# framecc.nomadsgalaxy.com

The landing page and the install address, served by one Cloudflare Worker (`framecc`):

- `/` is `index.html`.
- `/install` is the repo's `install` script, so the one-line install is
  `curl -fsSL https://framecc.nomadsgalaxy.com/install | sh`.
- `/dl/<file>` forwards to the latest GitHub release's file (`cc-install-<arch>`, `cc-host-<arch>`,
  `SHA256SUMS`). That only works once the repo is public and has a release.

To deploy, bundle the page and the script into the Worker and upload it as a module:

```
node -e "const f=require('fs');f.writeFileSync('/tmp/framecc.js',f.readFileSync('site/worker.js','utf8')
  .replace('\"__PAGE__\"',JSON.stringify(f.readFileSync('site/index.html','utf8')))
  .replace('\"__INSTALL__\"',JSON.stringify(f.readFileSync('install','utf8'))))"
npx wrangler deploy /tmp/framecc.js --name framecc --compatibility-date 2026-10-01
```

The custom domain `framecc.nomadsgalaxy.com` is attached to the `framecc` Worker in Cloudflare.
Redeploy whenever `index.html` or `install` changes.
