const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')

/** The store uses plain refs/computed, so a tiny stand-in is enough. */
function load() {
  const compiled = ts.transpileModule(fs.readFileSync('src/stores/checkout.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  const context = {
    exports: {},
    require: name => {
      const mocks = {
        pinia: { defineStore: (_id, setup) => setup },
        // Pinia unwraps computed refs on the store, so evaluate the factory once:
        // the returned closure still reads the current state on every call.
        vue: { ref: value => ({ value }), computed: factory => factory() },
      }
      assert.ok(name in mocks, name)
      return mocks[name]
    },
  }
  vm.runInNewContext(compiled, context)
  return unwrapRefs(context.exports.useCheckoutStore())
}

/** Pinia unwraps refs on a setup store; mirror that so the test sees the real API. */
function unwrapRefs(store) {
  const result = {}
  for (const [key, value] of Object.entries(store)) {
    const isRef = value && typeof value === 'object' && Object.keys(value).length === 1 && 'value' in value
    if (!isRef) {
      result[key] = value
      continue
    }
    Object.defineProperty(result, key, {
      get: () => value.value,
      set: next => { value.value = next },
      enumerable: true,
    })
  }
  return result
}

test('a checkout becomes resumable only after it is cancelled or fails', () => {
  const store = load()
  assert.equal(store.resumable('/wc'), false, 'nothing started yet')

  store.begin('http://host/repo', '/wc', 12)
  assert.equal(store.running('/wc'), true)
  assert.equal(store.resumable('/wc'), false, 'a running checkout is not resumable')
  assert.equal(store.running('/other'), false, 'another workspace is untouched')

  store.markCancelled()
  assert.equal(store.running('/wc'), false)
  assert.equal(store.resumable('/wc'), true)
  assert.equal(store.session.url, 'http://host/repo', 'the resume keeps the source URL')
  assert.equal(store.session.revision, 12)

  store.begin('http://host/repo', '/wc')
  store.markFailed()
  assert.equal(store.resumable('/wc'), true)

  store.begin('http://host/repo', '/wc')
  store.complete()
  assert.equal(store.resumable('/wc'), false, 'a finished checkout needs no resume')
  assert.equal(store.running('/wc'), false)
})

test('the session is scoped to its own path and can be discarded', () => {
  const store = load()
  store.begin('http://host/repo', '/wc')
  store.markCancelled()
  assert.equal(store.resumable('/other'), false)

  store.clear()
  assert.equal(store.session, null)
  assert.equal(store.resumable('/wc'), false)

  // Marking without a session must not create one.
  store.markCancelled()
  assert.equal(store.session, null)
})

test('starting again replaces the previous session', () => {
  const store = load()
  store.begin('http://host/one', '/a')
  store.markCancelled()
  store.begin('http://host/two', '/b', 7)
  assert.equal(store.session.path, '/b')
  assert.equal(store.session.phase, 'running')
  assert.equal(store.resumable('/a'), false)
  assert.equal(store.running('/b'), true)
})
