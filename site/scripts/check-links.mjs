// Checks the built site in dist/: every link inside a page's content to another
// page of the site must reach a page that exists, and its #anchor a heading on
// that page. Run after `astro build` (part of `npm run build`).

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const dist = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../dist');
const base = (process.env.BASE_PATH ?? '/jihoo').replace(/\/?$/, '/');

const html = new Map();
(function walk(dir) {
  for (const f of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, f.name);
    if (f.isDirectory()) walk(p);
    else if (f.name.endsWith('.html')) html.set(p, fs.readFileSync(p, 'utf-8'));
  }
})(dist);

const ids = new Map();
const idsOf = (file) => {
  if (!ids.has(file)) ids.set(file, new Set([...html.get(file).matchAll(/\sid="([^"]+)"/g)].map((m) => m[1])));
  return ids.get(file);
};

const problems = [];
for (const [file, page] of html) {
  const url = base + path.relative(dist, file).replace(/index\.html$/, '').split(path.sep).join('/');
  // Only the content: the sidebar and header are Starlight's own.
  const content = page.match(/<main[\s\S]*<\/main>/)?.[0] ?? '';
  for (const [, href] of content.matchAll(/<a\s[^>]*href="([^"]+)"/g)) {
    if (/^[a-z]+:/.test(href)) continue;
    const target = new URL(href.replace(/&amp;/g, '&'), `http://site${url}`);
    const p = decodeURIComponent(target.pathname);
    if (!p.startsWith(base)) {
      problems.push(`${url}: ${href} is outside the site (${base})`);
      continue;
    }
    const rel = p.slice(base.length);
    const candidates = [path.join(dist, rel, 'index.html'), path.join(dist, rel)];
    const found = candidates.find((c) => fs.existsSync(c) && fs.statSync(c).isFile());
    if (!found) {
      problems.push(`${url}: ${href}: no such page`);
    } else if (target.hash && html.has(found) && !idsOf(found).has(decodeURIComponent(target.hash.slice(1)))) {
      problems.push(`${url}: ${href}: no such anchor`);
    }
  }
}

if (problems.length) {
  console.error(problems.map((p) => `broken link: ${p}`).join('\n'));
  process.exit(1);
}
console.log(`checked links in ${html.size} pages`);
