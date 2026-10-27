const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')

function load() {
  const compiled = ts.transpileModule(fs.readFileSync('src/utils/checkoutProgress.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  const context = { exports: {} }
  vm.runInNewContext(compiled, context)
  return context.exports
}

const {
  VISIBLE_LINE_LIMIT,
  classifyCheckoutLine,
  countedItems,
  createProgressState,
  emptyCounters,
  pushCheckoutLine,
} = load()

test('svn status letters map to counters, prose does not', () => {
  assert.equal(classifyCheckoutLine('A    trunk/file.txt'), 'added')
  assert.equal(classifyCheckoutLine('U    trunk/file.txt'), 'updated')
  assert.equal(classifyCheckoutLine('G    trunk/file.txt'), 'updated')
  assert.equal(classifyCheckoutLine('D    trunk/old.txt'), 'deleted')
  assert.equal(classifyCheckoutLine('C    trunk/conflict.txt'), 'conflicted')
  assert.equal(classifyCheckoutLine('R    trunk/replaced.txt'), 'restored')
  for (const prose of [
    'Checked out revision 7394.',
    'Updating \'trunk\':',
    'svn: E175009: The XML response contains invalid XML',
    '',
    'Apath-without-separator.txt',
  ]) {
    assert.equal(classifyCheckoutLine(prose), null, prose)
  }
})

test('counters accumulate while the visible window stays bounded', () => {
  const state = createProgressState()
  pushCheckoutLine(state, 'out', 'A    one.txt')
  pushCheckoutLine(state, 'out', 'U    two.txt')
  pushCheckoutLine(state, 'err', 'svn: E175009: invalid XML')
  pushCheckoutLine(state, 'status', '第 2/3 次尝试')
  assert.equal(state.total, 4)
  assert.equal(state.lines.length, 4)
  assert.equal(state.counters.added, 1)
  assert.equal(state.counters.updated, 1)
  assert.equal(countedItems(state.counters), 2)

  for (let i = 0; i < VISIBLE_LINE_LIMIT + 50; i++) {
    pushCheckoutLine(state, 'out', `A    file-${i}.txt`)
  }
  assert.equal(state.total, 4 + VISIBLE_LINE_LIMIT + 50)
  assert.equal(state.lines.length, VISIBLE_LINE_LIMIT)
  // The tail is kept, so the newest lines stay visible.
  assert.equal(state.lines[state.lines.length - 1].text, `A    file-${VISIBLE_LINE_LIMIT + 49}.txt`)
  assert.equal(state.counters.added, 1 + VISIBLE_LINE_LIMIT + 50)
})

test('a fresh state starts empty and independent', () => {
  const first = createProgressState()
  const second = createProgressState()
  pushCheckoutLine(first, 'out', 'A    x')
  assert.equal(first.counters.added, 1)
  assert.equal(second.counters.added, 0)
  assert.deepEqual(second.counters, emptyCounters())
})

test('custom limits keep the most recent lines only', () => {
  const state = createProgressState()
  for (let i = 0; i < 5; i++) pushCheckoutLine(state, 'out', `A    f${i}`, 3)
  assert.equal(state.lines.length, 3)
  assert.deepEqual(Array.from(state.lines.map(line => line.text)), ['A    f2', 'A    f3', 'A    f4'])
})
