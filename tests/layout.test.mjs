// Run with: node --test tests/   (after `npx tsc -p .` so out/ is current)
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildTree } from '../web/out/app/src/tree.js';
import { layoutRepo, placeClusters } from '../web/out/app/src/layout.js';

const files = ['README.md', 'src/main.ts', 'src/util/a.ts', 'src/util/b.ts', 'src/util/c.ts', 'docs/x.md', 'docs/deep/y/z.md'];

test('buildTree counts files and folders', () => {
  const t = buildTree(files, 'demo');
  assert.equal(t.fileCount, 7);
  assert.equal(t.folderCount, 5); // src, src/util, docs, docs/deep, docs/deep/y
  assert.deepEqual(t.children.map((c) => c.name), ['docs', 'src', 'README.md']);
});

test('layout is deterministic', () => {
  const a = layoutRepo(buildTree(files, 'demo'));
  const b = layoutRepo(buildTree([...files].reverse(), 'demo'));
  assert.equal(a.nodes.length, 13);
  const pos = (l) => Object.fromEntries(l.nodes.map((n) => [n.node.path, [n.x, n.y, n.z].map((v) => v.toFixed(4)).join()]));
  assert.deepEqual(pos(a), pos(b));
});

test('adding a file does not move its siblings (within a size step)', () => {
  const a = layoutRepo(buildTree(files, 'demo'));
  const b = layoutRepo(buildTree([...files, 'src/util/d.ts'], 'demo'));
  const rel = (l, p) => {
    const n = l.nodes.find((x) => x.node.path === p);
    const parent = l.nodes[n.parent];
    return [n.x - parent.x, n.y - parent.y, n.z - parent.z].map((v) => v.toFixed(3)).join();
  };
  assert.equal(rel(a, 'src/util/a.ts'), rel(b, 'src/util/a.ts'));
});

test('clusters never overlap', () => {
  const radii = [40, 10, 25, 60, 5];
  const c = placeClusters(radii, ['a', 'b', 'c', 'd', 'e']);
  for (let i = 0; i < c.length; i++) for (let j = i + 1; j < c.length; j++) {
    const d = Math.hypot(c[i][0] - c[j][0], c[i][1] - c[j][1], c[i][2] - c[j][2]);
    assert.ok(d > radii[i] + radii[j], `clusters ${i} and ${j} overlap`);
  }
});

test('clusters keep their place when a repo grows', () => {
  const keys = ['a', 'b', 'c'];
  const first = placeClusters([30, 12, 20], keys);
  const prev = new Map(keys.map((k, i) => [k, first[i]]));
  const grown = placeClusters([30, 14, 20], keys, prev); // b grew a little
  assert.deepEqual(grown, first);
});

test('removing a folder leaves its siblings in place', () => {
  const base = ['a/1.ts', 'b/1.ts', 'c/1.ts', 'd/1.ts', 'e/1.ts', 'f/1.ts'];
  const rel = (files, path) => {
    const l = layoutRepo(buildTree(files, 'demo'));
    const root = l.nodes[0];
    const n = l.nodes.find((x) => x.node.path === path);
    return [n.x - root.x, n.y - root.y, n.z - root.z].map((v) => v.toFixed(3)).join();
  };
  const without = base.filter((f) => !f.startsWith('c/'));
  for (const p of ['a', 'b', 'd', 'e', 'f']) assert.equal(rel(base, p), rel(without, p), `${p} moved`);
});
