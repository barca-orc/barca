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
points assets at `dist/client` after `astro build`. Release tags deploy through the `deploy-site`
job in `.github/workflows/release.yml` with `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`.
Between releases, deploy from this directory after `npx wrangler login` or with those variables set.
