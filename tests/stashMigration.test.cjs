const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')

function load() {
  const compiled = ts.transpileModule(fs.readFileSync('src/stores/stash.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  const context = {
    exports: {},
    require: name => {
      const mocks = {
        pinia: { defineStore: (_id, setup) => setup },
        vue: { ref: value => ({ value }), computed: factory => factory },
        '@/api/svn': {
          deleteStash: async () => {},
          importLegacyStashes: async () => 0,
          listStashes: async () => [],
          saveStash: async () => {},
        },
      }
      assert.ok(name in mocks, name)
      return mocks[name]
    },
  }
  vm.runInNewContext(compiled, context)
  return context.exports
}

const { splitLegacyEntries } = load()

const entry = (id, workspacePath) => ({
  id,
  name: id,
  workspacePath,
  createdAt: '2026-01-01T00:00:00.000Z',
  patch: 'diff',
  files: ['a.txt'],
  hunkCount: 1,
})

test('only records of the requested workspace are migrated', () => {
  const raw = [entry('a', '/wc'), entry('b', '/other'), entry('c', '/wc')]
  const { imported, remaining } = splitLegacyEntries(raw, '/wc')
  assert.deepEqual(Array.from(imported.map(item => item.id)), ['a', 'c'])
  assert.deepEqual(Array.from(remaining.map(item => item.id)), ['b'])
})

test('records of other workspaces stay in local storage for their own load', () => {
  const { imported, remaining } = splitLegacyEntries([entry('b', '/other')], '/wc')
  assert.equal(imported.length, 0)
  assert.equal(remaining.length, 1)
})

test('malformed records are dropped instead of retried forever', () => {
  const raw = [null, 'text', { id: 'x' }, { ...entry('ok', '/wc'), hunkCount: undefined }]
  const { imported, remaining } = splitLegacyEntries(raw, '/wc')
  assert.equal(imported.length, 0)
  assert.equal(remaining.length, 0, 'invalid entries are neither imported nor kept')
})

test('a non-array payload migrates nothing', () => {
  for (const raw of [null, undefined, {}, 'nope', 3]) {
    const { imported, remaining } = splitLegacyEntries(raw, '/wc')
    assert.equal(imported.length, 0)
    assert.equal(remaining.length, 0)
  }
})

test('workspace paths are matched exactly, not by prefix', () => {
  const raw = [entry('a', '/wc'), entry('b', '/wc/sub')]
  const { imported, remaining } = splitLegacyEntries(raw, '/wc')
  assert.deepEqual(Array.from(imported.map(item => item.id)), ['a'])
  assert.deepEqual(Array.from(remaining.map(item => item.id)), ['b'])
})
