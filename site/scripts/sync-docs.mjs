// Builds pages for the website from the repository's own documentation, so that
// `docs/` and `examples/` stay the single source of truth:
//
//   docs/design.md  -> language/*.md and internals/*.md, one page per `##` section
//   docs/jir.md     -> reference/jir.md
//   examples/*.jh   -> examples/*.md, the leading comment as text, then the code
//
// The output directories are generated (and git-ignored); edit the sources.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const site = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const repo = path.resolve(site, '..');
const out = path.join(site, 'src/content/docs');
const github = 'https://github.com/jihoo12/jihoo/blob/main';

// Sections of design.md that describe how jihoo is built, not the language.
const INTERNALS = new Set(['GC', 'Why the IR is a text file', 'Testing', 'Roadmap']);

function reset(dir) {
  fs.rmSync(path.join(out, dir), { recursive: true, force: true });
  fs.mkdirSync(path.join(out, dir), { recursive: true });
}

function slug(title) {
  return title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
}

function page(file, { title, description, order }, body) {
  const front = ['---', `title: ${JSON.stringify(title)}`];
  if (description) front.push(`description: ${JSON.stringify(description)}`);
  if (order !== undefined) front.push('sidebar:', `  order: ${order}`);
  front.push('---', '');
  fs.writeFileSync(path.join(out, file), front.join('\n') + body.trim() + '\n');
}

/** Splits markdown into the text before the first `## ` heading and the sections. */
function sections(md) {
  const lines = md.split('\n');
  const result = { intro: [], sections: [] };
  let fence = false;
  for (const line of lines) {
    if (line.startsWith('```')) fence = !fence;
    if (!fence && line.startsWith('## ')) {
      result.sections.push({ title: line.slice(3).trim(), lines: [] });
    } else if (result.sections.length) {
      result.sections.at(-1).lines.push(line);
    } else {
      result.intro.push(line);
    }
  }
  return result;
}

/** One level up (`###` -> `##`), outside code blocks: each section is a page. */
function promote(lines) {
  let fence = false;
  return lines
    .map((line) => {
      if (line.startsWith('```')) fence = !fence;
      return !fence && line.startsWith('### ') ? line.slice(1) : line;
    })
    .join('\n');
}

/** Turns repository paths in code spans into links to the source on GitHub. */
function linkPaths(md) {
  return md.replace(/`((?:crates|lib|backend-llvm|examples|tests|docs)\/[\w./-]+)`/g, (m, p) =>
    fs.existsSync(path.join(repo, p)) ? `[\`${p}\`](${github}/${p})` : m,
  );
}

function firstParagraph(md) {
  return md
    .split(/\n\s*\n/)
    .map((p) => p.trim())
    .find((p) => p && !p.startsWith('#') && !p.startsWith('```'))
    ?.replace(/\s+/g, ' ')
    .replace(/[`*]/g, '');
}

// ---- docs/design.md ----

reset('language');
reset('internals');
const design = sections(fs.readFileSync(path.join(repo, 'docs/design.md'), 'utf-8'));
const intro = linkPaths(design.intro.filter((l) => !l.startsWith('# ')).join('\n'));
page('internals/architecture.md', { title: 'Architecture', order: 0 }, intro);
design.sections.forEach((s, i) => {
  const dir = INTERNALS.has(s.title) ? 'internals' : 'language';
  const body = linkPaths(promote(s.lines));
  page(`${dir}/${slug(s.title)}.md`, { title: s.title, description: firstParagraph(body), order: i + 1 }, body);
});

// ---- docs/jir.md ----

reset('reference');
const jir = fs.readFileSync(path.join(repo, 'docs/jir.md'), 'utf-8');
const jirTitle = jir.match(/^# (.*)$/m)[1];
page('reference/jir.md', { title: 'JIR', description: jirTitle }, linkPaths(jir.replace(/^# .*\n/, '')));

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
  let start = lines[0].startsWith('#![') ? 1 : 0;
  const comment = [];
  while (start < lines.length && lines[start].startsWith('//')) {
    comment.push(lines[start].replace(/^\/\/ ?/, ''));
    start++;
  }
  const commands = comment.filter((l) => /^\s+jihoo /.test(l)).map((l) => l.trim());
  const prose = comment.filter((l) => !/^\s+jihoo /.test(l)).join('\n').trim();
  const name = file.replace(/\.jh$/, '');
  // The header comment is shown as text above, so the code starts after it.
  const code = [...(lines[0].startsWith('#![') ? [lines[0]] : []), ...lines.slice(start)]
    .join('\n')
    .replace(/^\s*\n/, '')
    .trimEnd();
  const body = [
    prose,
    commands.length ? '```sh\n' + commands.join('\n') + '\n```' : '',
    '```jihoo title="' + `examples/${file}` + '"\n' + code + '\n```',
    `[View on GitHub](${github}/examples/${file})`,
  ]
    .filter(Boolean)
    .join('\n\n');
  page(`examples/${name}.md`, { title: name, description: prose.split('\n')[0], order: i }, body);
});

console.log(`synced ${design.sections.length + 1} design pages, the JIR reference, ${examples.length} examples`);
