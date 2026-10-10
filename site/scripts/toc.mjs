// Reads the table of contents in docs/README.md, which decides which files of
// docs/ become pages, where they go, and the order of the sidebar:
//
//   ## Language                  a sidebar group; its pages go to language/
//   ### Basics                   a subgroup (optional)
//   - [Profiles](language/profiles.md)
//
// A page's slug is the group's directory plus the file's name, wherever the file
// is in docs/: `[JIR](jir.md)` under `## Reference` is reference/jir.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
export const docs = path.join(repo, 'docs');

export function slugify(title) {
  return title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
}

/**
 * @returns {{ groups: { label: string, dir: string, items: ({ label: string, items: Page[] } | Page)[] }[], pages: Page[] }}
 * @typedef {{ title: string, file: string, slug: string }} Page   file: relative to docs/
 */
export function readToc() {
  const groups = [];
  const pages = [];
  let group = null;
  let sub = null;
  for (const line of fs.readFileSync(path.join(docs, 'README.md'), 'utf-8').split('\n')) {
    let m;
    if ((m = line.match(/^## (.+)$/))) {
      group = { label: m[1].trim(), dir: slugify(m[1]), items: [] };
      sub = null;
      groups.push(group);
    } else if ((m = line.match(/^### (.+)$/)) && group) {
      sub = { label: m[1].trim(), items: [] };
      group.items.push(sub);
    } else if ((m = line.match(/^- \[([^\]]+)\]\(([^)]+\.md)\)\s*$/)) && group) {
      const file = path.normalize(m[2]);
      const page = { title: m[1], file, slug: `${group.dir}/${path.basename(file, '.md')}` };
      if (pages.some((p) => p.slug === page.slug)) throw new Error(`docs/README.md: two pages are ${page.slug}`);
      pages.push(page);
      (sub ?? group).items.push(page);
    }
  }
  return { groups, pages };
}

/** The sidebar entries for one group of the table of contents. */
export function sidebarGroup(group) {
  const entry = (item) =>
    'slug' in item ? item.slug : { label: item.label, items: item.items.map(entry) };
  return { label: group.label, items: group.items.map(entry) };
}
