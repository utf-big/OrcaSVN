const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')

function load() {
  const compiled = ts.transpileModule(fs.readFileSync('src/utils/workspaceAttach.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  const context = { exports: {}, setTimeout, Promise }
  vm.runInNewContext(compiled, context)
  return context.exports.waitForWorkingCopy
}

const instantSleep = async () => {}

test('resolves once the checkout has created the working copy', async () => {
  const waitForWorkingCopy = load()
  const answers = [false, false, true]
  let probes = 0
  const ready = await waitForWorkingCopy(async () => answers[probes++], { intervalMs: 0, sleep: instantSleep })
  assert.equal(ready, true)
  assert.equal(probes, 3, 'it stops probing as soon as .svn exists')
})

test('gives up when the directory never becomes a working copy', async () => {
  const waitForWorkingCopy = load()
  let probes = 0
  const ready = await waitForWorkingCopy(async () => { probes++; return false }, { attempts: 4, intervalMs: 0, sleep: instantSleep })
  assert.equal(ready, false)
  assert.equal(probes, 4)
})

test('stops probing when the checkout was cancelled or the page left', async () => {
  const waitForWorkingCopy = load()
  let probes = 0
  let stop = false
  const ready = await waitForWorkingCopy(async () => { probes++; stop = true; return false }, {
    attempts: 10,
    intervalMs: 0,
    sleep: instantSleep,
    shouldStop: () => stop,
  })
  assert.equal(ready, false)
  assert.equal(probes, 1, 'no further probes after the stop flag is set')
})

test('a probe that throws keeps the wait alive instead of failing', async () => {
  const waitForWorkingCopy = load()
  let probes = 0
  const ready = await waitForWorkingCopy(async () => {
    probes += 1
    if (probes < 3) throw new Error('directory is being created')
    return true
  }, { intervalMs: 0, sleep: instantSleep })
  assert.equal(ready, true)
  assert.equal(probes, 3)
})
