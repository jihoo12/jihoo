// @ts-check
import fs from 'node:fs';
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// Syntax highlighting for ```jihoo code blocks.
const jihoo = JSON.parse(fs.readFileSync(new URL('./src/jihoo.tmLanguage.json', import.meta.url), 'utf-8'));

// GitHub Pages serves the site under /<repository>/. The deploy workflow passes
// the exact origin and path; these defaults match github.com/jihoo12/jihoo.
export default defineConfig({
  site: process.env.SITE_URL ?? 'https://jihoo12.github.io',
  base: process.env.BASE_PATH ?? '/jihoo',
  integrations: [
    starlight({
      title: 'jihoo',
      description:
        'An easy programming language with a garbage-collected VM, freestanding LLVM builds, compile-time evaluation and macros.',
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/jihoo12/jihoo' }],
      customCss: ['./src/styles/custom.css'],
      expressiveCode: { shiki: { langs: [jihoo] } },
      sidebar: [
        { label: 'Start here', items: ['start/getting-started'] },
        { label: 'Language', items: [{ autogenerate: { directory: 'language' } }] },
        { label: 'Examples', items: [{ autogenerate: { directory: 'examples' } }] },
        { label: 'Reference', items: [{ autogenerate: { directory: 'reference' } }] },
        { label: 'Internals', items: [{ autogenerate: { directory: 'internals' } }] },
      ],
    }),
  ],
});
