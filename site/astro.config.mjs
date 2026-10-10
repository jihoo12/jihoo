// @ts-check
import fs from 'node:fs';
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { readToc, sidebarGroup } from './scripts/toc.mjs';

// Syntax highlighting for ```jihoo code blocks.
const jihoo = JSON.parse(fs.readFileSync(new URL('./src/jihoo.tmLanguage.json', import.meta.url), 'utf-8'));

// The sidebar follows the table of contents in docs/README.md; the examples come
// right after the language.
const toc = readToc().groups.map(sidebarGroup);
const examples = { label: 'Examples', items: [{ autogenerate: { directory: 'examples' } }] };
const sidebar = [
  { label: 'Start here', items: ['start/getting-started'] },
  ...toc.flatMap((g) => (g.label === 'Language' ? [g, examples] : [g])),
];

// GitHub Pages serves the site under /<repository>/. The deploy workflow passes
// the exact origin and path; these defaults match github.com/jihoo12/jihoo.
const base = process.env.BASE_PATH ?? '/jihoo';

// Pages that moved when the docs were split into one file per topic.
const moved = {
  'language/structs-and-pointers': 'language/structs',
  'language/enums-and-match': 'language/enums',
  'internals/gc': 'internals/vm',
  'internals/why-the-ir-is-a-text-file': 'internals/architecture',
};

export default defineConfig({
  site: process.env.SITE_URL ?? 'https://jihoo12.github.io',
  base,
  redirects: Object.fromEntries(
    Object.entries(moved).map(([from, to]) => [`/${from}`, `${base.replace(/\/$/, '')}/${to}/`]),
  ),
  integrations: [
    starlight({
      title: 'jihoo',
      description:
        'An easy programming language with a garbage-collected VM, native and freestanding LLVM builds, compile-time evaluation and macros.',
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/jihoo12/jihoo' }],
      customCss: ['./src/styles/custom.css'],
      expressiveCode: { shiki: { langs: [jihoo] } },
      sidebar,
    }),
  ],
});
