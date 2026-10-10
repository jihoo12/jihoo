# jihoo website

The documentation site, built with [Astro](https://astro.build) and
[Starlight](https://starlight.astro.build), and published to GitHub Pages by
[`.github/workflows/pages.yml`](../.github/workflows/pages.yml).

Most pages are generated, so that `docs/` and `examples/` stay the only source:
`npm run sync` (part of `dev` and `build`) runs
[`scripts/sync-docs.mjs`](scripts/sync-docs.mjs), which turns

- every Markdown file in `docs/` into a page. The table of contents in
  [`docs/README.md`](../docs/README.md) decides where each one goes and the order
  of the sidebar ([`scripts/toc.mjs`](scripts/toc.mjs) reads it, for the sync
  script and for `astro.config.mjs`). Relative links between the files become
  links between pages;
- `examples/*.jh` into example pages, with their header comment as the text.

To add a page, write `docs/<group>/<name>.md` starting with a `# Title` line and
list it in `docs/README.md`. The sync fails on a page missing from the table of
contents or a link to a file that does not exist, and `npm run build` then runs
[`scripts/check-links.mjs`](scripts/check-links.mjs), which fails on a link or
`#anchor` that leads nowhere in the built site.

Only the landing page and Getting started are written here, in
`src/content/docs/`. Pages that moved get a redirect in `astro.config.mjs`.
Syntax highlighting for jihoo code comes from
[`src/jihoo.tmLanguage.json`](src/jihoo.tmLanguage.json).

```sh
nix develop .#site     # Node.js
cd site
npm ci
npm run dev            # http://localhost:4321/jihoo/
npm run build          # into dist/
```

`SITE_URL` and `BASE_PATH` override the default address
(`https://jihoo12.github.io` + `/jihoo`); the deploy workflow sets them from the
repository's Pages settings.

To publish, set the repository's **Settings → Pages → Source** to
**GitHub Actions** once; every push to `main` that touches `site/`, `docs/` or
`examples/` then rebuilds the site.
