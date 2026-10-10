// Builds pages for the website from the repository's own documentation, so that
// `docs/` and `examples/` stay the single source of truth:
//
//   docs/**/*.md   -> one page each, placed as docs/README.md's table of contents says
//   examples/*.jh  -> examples/*.md, the leading comment as text, then the code
//
// Relative links between the Markdown files become links between pages, links
// to other files of the repository point to GitHub, and a link to a file that
// does not exist stops the build. The output directories are generated (and
// git-ignored); edit the sources.

import fs from 'node:fs';
import path from 'node:path';
import { docs, readToc, repo } from './toc.mjs';

const site = path.join(repo, 'site');
const out = path.join(site, 'src/content/docs');
const github = 'https://github.com/jihoo12/jihoo/blob/main';
const { groups, pages } = readToc();
const errors = [];

function reset(dir) {
  fs.rmSync(path.join(out, dir), { recursive: true, force: true });
  fs.mkdirSync(path.join(out, dir), { recursive: true });
}

function writePage(slug, { title, description, sidebar }, body) {
  const front = ['---', `title: ${JSON.stringify(title)}`];
  if (description) front.push(`description: ${JSON.stringify(description)}`);
  if (sidebar) front.push(`sidebar: ${JSON.stringify(sidebar)}`);
  front.push('---', '');
  fs.mkdirSync(path.dirname(path.join(out, slug)), { recursive: true });
  fs.writeFileSync(path.join(out, `${slug}.md`), front.join('\n') + body.trim() + '\n');
}

/** The page a repository file is shown on, if it has one. */
function pageOf(abs) {
  const rel = path.relative(repo, abs);
  if (rel.startsWith('docs' + path.sep)) {
    return pages.find((p) => path.join('docs', p.file) === rel)?.slug;
  }
  const handwritten = rel.match(/^site\/src\/content\/docs\/(.+)\.mdx?$/);
  if (handwritten) return handwritten[1].replace(/(^|\/)index$/, '');
  const example = rel.match(/^examples\/([\w-]+)\.jh$/);
  if (example) return `examples/${example[1]}`;
  return undefined;
}

/** A link from page `from` to page `to`, relative so that any base path works. */
function relative(from, to, hash = '') {
  return '../'.repeat(from.split('/').length) + (to ? `${to}/` : '') + hash;
}

/** Applies `f` to the text outside fenced code blocks. */
function outsideCode(md, f) {
  return md
    .split(/(^```[\s\S]*?^```)/m)
    .map((part, i) => (i % 2 ? part : f(part)))
    .join('');
}

/** Rewrites `[text](target)` links in `md`, a file at `file` shown as page `slug`. */
function links(md, file, slug) {
  return outsideCode(md, (text) =>
    // Inline code is left alone: `[x](y)` there is not a link.
    text.replace(/(`[^`\n]*`)|\[([^\]\n]*)\]\(([^)\s]+)\)/g, (m, code, label, target) => {
      if (code || /^([a-z]+:|#)/.test(target)) return m;
      const [p, hash = ''] = target.split(/(?=#)/);
      const abs = path.resolve(path.dirname(file), p);
      if (!fs.existsSync(abs)) {
        errors.push(`${path.relative(repo, file)}: link to a missing file: ${target}`);
        return m;
      }
      const to = pageOf(abs);
      if (to !== undefined) return `[${label}](${relative(slug, to, hash)})`;
      if (abs.endsWith('.md') && abs.startsWith(docs)) {
        errors.push(`${path.relative(repo, file)}: ${target} is not in docs/README.md`);
        return m;
      }
      return `[${label}](${github}/${path.relative(repo, abs)}${hash})`;
    }),
  );
}

/** Turns repository paths in code spans into links: to their page, or to GitHub. */
function linkPaths(md, slug) {
  return outsideCode(md, (text) =>
    text.replace(/(?<!\[)`((?:crates|lib|backend-llvm|examples|tests|docs|site)\/[\w./-]+)`/g, (m, p) => {
      const abs = path.join(repo, p);
      if (!fs.existsSync(abs)) return m;
      const to = pageOf(abs);
      return to !== undefined ? `[\`${p}\`](${relative(slug, to)})` : `[\`${p}\`](${github}/${p})`;
    }),
  );
}

function firstParagraph(md) {
  return md
    .split(/\n\s*\n/)
    .map((p) => p.trim())
    .find((p) => p && !/^(#|```|\||-|\d+\.)/.test(p))
    ?.replace(/\s+/g, ' ')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/[`*]/g, '');
}

// ---- docs/ ----

for (const group of groups) reset(group.dir);

for (const page of pages) {
  const file = path.join(docs, page.file);
  if (!fs.existsSync(file)) {
    errors.push(`docs/README.md: ${page.file} does not exist`);
    continue;
  }
  const md = fs.readFileSync(file, 'utf-8');
  // The `# Title` line is the page title; the TOC names the page in the sidebar.
  const title = md.match(/^# (.+)$/m)?.[1] ?? page.title;
  const body = linkPaths(links(md.replace(/^# .*\n/, ''), file, page.slug), page.slug);
  const sidebar = title === page.title ? undefined : { label: page.title };
  writePage(page.slug, { title, description: firstParagraph(body), sidebar }, body);
}

// Every page in docs/ must be reachable from the table of contents.
(function walk(dir) {
  for (const f of fs.readdirSync(dir, { withFileTypes: true })) {
    const abs = path.join(dir, f.name);
    const rel = path.relative(docs, abs);
    if (f.isDirectory()) walk(abs);
    else if (f.name.endsWith('.md') && rel !== 'README.md' && !pages.some((p) => p.file === rel)) {
      errors.push(`docs/${rel} is not in the table of contents in docs/README.md`);
    }
  }
})(docs);

// ---- examples/*.jh ----

reset('examples');
const examples = fs.readdirSync(path.join(repo, 'examples')).filter((f) => f.endsWith('.jh')).sort();
// Simple programs first, then alphabetical.
const first = ['hello.jh', 'fib.jh'];
const rank = (f) => (first.includes(f) ? first.indexOf(f) : first.length);
examples.sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
examples.forEach((file, i) => {
  const src = fs.readFileSync(path.join(repo, 'examples', file), 'utf-8');
  // The comment block at the top (after an optional `#![...]`) explains the example.
  const lines = src.split('\n');
  const attr = lines[0].match(/^#!\[(\w+)\]/)?.[1];
  let start = attr ? 1 : 0;
  const comment = [];
  while (start < lines.length && lines[start].startsWith('//')) {
    comment.push(lines[start].replace(/^\/\/ ?/, ''));
    start++;
  }
  const commands = comment.filter((l) => /^\s+jihoo /.test(l)).map((l) => l.trim());
  const prose = comment.filter((l) => !/^\s+jihoo /.test(l)).join('\n').trim();
  const name = file.replace(/\.jh$/, '');
  const slug = `examples/${name}`;
  // The header comment is shown as text above, so the code starts after it.
  const code = [...(attr ? [lines[0]] : []), ...lines.slice(start)]
    .join('\n')
    .replace(/^\s*\n/, '')
    .trimEnd();
  const body = [
    linkPaths(prose, slug),
    commands.length ? '```sh\n' + commands.join('\n') + '\n```' : '',
    '```jihoo title="' + `examples/${file}` + '"\n' + code + '\n```',
    `[View on GitHub](${github}/examples/${file})`,
  ]
    .filter(Boolean)
    .join('\n\n');
  // Compiled examples are marked with their profile in the sidebar.
  const sidebar = { order: i, ...(attr && { badge: { text: attr, variant: attr === 'native' ? 'note' : 'caution' } }) };
  writePage(slug, { title: name, description: prose.split('\n')[0], sidebar }, body);
});

if (errors.length) {
  console.error(errors.map((e) => `error: ${e}`).join('\n'));
  process.exit(1);
}
console.log(`synced ${pages.length} pages from docs/ and ${examples.length} examples`);
