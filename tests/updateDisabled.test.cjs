const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')

function load(fetchCalls) {
  const compiled = ts.transpileModule(fs.readFileSync('src/composables/useUpdateCheck.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, resolveJsonModule: true, esModuleInterop: true },
  }).outputText
  const context = {
    exports: {},
    fetch: (...args) => { fetchCalls.push(args); return Promise.reject(new Error('network must not be used')) },
    AbortController: class { constructor() { this.signal = undefined } abort() {} },
    window: { setTimeout: () => 0, clearTimeout: () => {} },
    require: name => {
      const mocks = {
        vue: { readonly: value => value, ref: value => ({ value }) },
        '@tauri-apps/plugin-shell': { open: async () => {} },
        '../../package.json': { version: '0.6.4' },
        '@/utils/version': { isNewerVersion: () => false },
      }
      assert.ok(name in mocks, name)
      return mocks[name]
    },
  }
  vm.runInNewContext(compiled, context)
  return context.exports
}

test('update checking is switched off in this derivative build', () => {
  assert.equal(load([]).AUTO_UPDATE_ENABLED, false)
})

test('neither the startup check nor a manual check reaches the network', async () => {
  const calls = []
  const { useUpdateCheck } = load(calls)
  const { state, checkForUpdates } = useUpdateCheck()
  assert.equal(state.value, 'disabled', 'the state reports the feature as off')

  await checkForUpdates()
  await checkForUpdates()
  assert.equal(calls.length, 0, 'no request was made')
  assert.equal(state.value, 'disabled')
})
