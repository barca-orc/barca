# barca

The Barca documentation site — [Astro](https://astro.build) +
[Starlight](https://starlight.astro.build), served from a [Cloudflare
Worker](https://developers.cloudflare.com/workers/) with static assets. Source content lives in
`src/content/docs/`.

## Develop

```bash
npm install
npm run dev       # http://localhost:4321
```

## Build and deploy

```bash
npm run build     # outputs to dist/ for the Cloudflare adapter
npm run preview   # build and serve locally with wrangler dev
npm run deploy    # build and wrangler deploy (production Worker + barca.sh routes)
```

`site/wrangler.jsonc` names the Worker (`barca`), attaches `barca.sh` and `www.barca.sh`, and
points assets at `dist/client` after `astro build`. `.github/workflows/site.yml` deploys it, with the `CLOUDFLARE_API_TOKEN` and
`CLOUDFLARE_ACCOUNT_ID` secrets:

- a commit on `main` that changes `site/` or `CHANGELOG.md` runs `wrangler deploy`, so barca.sh
  serves main a few minutes after a merge;
- a pull request from a branch of this repository that changes them uploads a preview version
  (`wrangler versions upload --preview-alias <branch>`) at
  `https://<branch>-barca.<account subdomain>.workers.dev`, which production does not serve; a
  PR comment links it and is updated on each push. PRs from forks get no preview.

Run the workflow by hand (Actions > Site > Run workflow) to redeploy a branch, or deploy from
this directory after `npx wrangler login` or with those variables set.
