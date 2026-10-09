# jihoo website

The documentation site, built with [Astro](https://astro.build) and
[Starlight](https://starlight.astro.build), and published to GitHub Pages by
[`.github/workflows/pages.yml`](../.github/workflows/pages.yml).

Most pages are generated, so that `docs/` and `examples/` stay the only source:
`npm run sync` (part of `dev` and `build`) runs
[`scripts/sync-docs.mjs`](scripts/sync-docs.mjs), which turns

- `docs/design.md` into one page per `##` section (Language and Internals),
- `docs/jir.md` into the JIR reference,
- `examples/*.jh` into example pages.

Only the landing page and Getting started are written here, in
`src/content/docs/`. Syntax highlighting for jihoo code comes from
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
