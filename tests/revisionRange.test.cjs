const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')

function load() {
  const compiled = ts.transpileModule(fs.readFileSync('src/utils/revisionRange.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  const context = { exports: {} }
  vm.runInNewContext(compiled, context)
  return context.exports
}

const { isValidRevisionRange, defaultMergeStart } = load()

test('a merge range must ascend and start at revision 1 or later', () => {
  assert.equal(isValidRevisionRange(1, 1), true)
  assert.equal(isValidRevisionRange(10, 12), true)
  assert.equal(isValidRevisionRange(12, 10), false)
  assert.equal(isValidRevisionRange(0, 5), false)
  assert.equal(isValidRevisionRange(5, 5.5), false)
  assert.equal(isValidRevisionRange(Number.NaN, 5), false)
})

test('the merge range starts at the working copy revision', () => {
  assert.equal(defaultMergeStart({ revision: 42 }), 42)
  assert.equal(defaultMergeStart({ revision: 0 }), 1)
  assert.equal(defaultMergeStart(null), 1)
})
