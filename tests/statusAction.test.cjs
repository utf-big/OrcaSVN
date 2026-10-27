const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')

function load() {
  const compiled = ts.transpileModule(fs.readFileSync('src/utils/statusAction.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  const context = { exports: {} }
  vm.runInNewContext(compiled, context)
  return context.exports
}

const {
  RESOLVE_STRATEGIES,
  isConflicted,
  canDeleteDuplicate,
  canRepairName,
  canResolve,
  canScheduleDelete,
  rowOperation,
  isResolveStrategy,
} = load()

const file = (status_code, prop_status = 'none') => ({ path: 'src/a.txt', status_code, prop_status })

test('the row action deletes unversioned targets and reverts everything else', () => {
  assert.equal(rowOperation(file('unversioned')), 'deleteUnversioned')
  for (const code of ['modified', 'added', 'deleted', 'replaced', 'missing', 'conflicted']) {
    assert.equal(rowOperation(file(code)), 'revert', code)
  }
})

test('only a missing file can be scheduled for deletion', () => {
  assert.equal(canScheduleDelete(file('missing')), true)
  for (const code of ['modified', 'added', 'deleted', 'replaced', 'unversioned', 'conflicted', 'normal']) {
    assert.equal(canScheduleDelete(file(code)), false, code)
  }
})

test('conflicts are recognised from either the entry or its properties', () => {
  assert.equal(isConflicted(file('conflicted')), true)
  assert.equal(isConflicted(file('modified', 'conflicted')), true)
  assert.equal(isConflicted(file('modified')), false)
  assert.equal(canResolve(file('conflicted')), true)
  assert.equal(canResolve(file('missing')), false)
})

test('resolve strategies stay limited to what svn resolve --accept supports', () => {
  assert.deepEqual([...RESOLVE_STRATEGIES], ['working', 'mine-full', 'theirs-full'])
  for (const strategy of RESOLVE_STRATEGIES) {
    assert.equal(isResolveStrategy(strategy), true, strategy)
  }
  for (const value of ['base', 'WORKING', '', null, 1, undefined]) {
    assert.equal(isResolveStrategy(value), false, String(value))
  }
  // The menu's first entry doubles as the fallback when no strategy is passed.
  assert.equal(RESOLVE_STRATEGIES[0], 'working')
})

test('only a name mismatch can be repaired by renaming', () => {
  assert.equal(canRepairName(file('name_mismatch')), true)
  for (const code of ['missing', 'modified', 'unversioned', 'conflicted']) {
    assert.equal(canRepairName(file(code)), false, code)
  }
})

test('a repository duplicate needs deletion, not a rename', () => {
  assert.equal(canDeleteDuplicate(file('name_duplicate')), true)
  for (const code of ['name_mismatch', 'missing', 'unversioned', 'modified']) {
    assert.equal(canDeleteDuplicate(file(code)), false, code)
  }
  // The two states never overlap: a duplicate is not repairable by renaming.
  assert.equal(canRepairName(file('name_duplicate')), false)
  assert.equal(canDeleteDuplicate(file('name_mismatch')), false)
})
