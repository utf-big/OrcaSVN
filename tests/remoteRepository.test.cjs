const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')

function load(svnInfo) {
  const compiled = ts.transpileModule(fs.readFileSync('src/composables/useRemoteRepository.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  const opened = []
  const context = {
    exports: {},
    require: name => {
      const mocks = {
        // A snapshot ref is enough here: the store never changes during a test.
        vue: { computed: factory => ({ value: factory() }) },
        '@tauri-apps/plugin-shell': {
          open: async url => {
            opened.push(url)
            if (url === 'https://fail.example/repo') throw new Error('no handler')
          },
        },
        '@/stores/workspace': { useWorkspaceStore: () => ({ svnInfo }) },
      }
      assert.ok(name in mocks, name)
      return mocks[name]
    },
  }
  vm.runInNewContext(compiled, context)
  return { ...context.exports.useRemoteRepository(), opened }
}

test('the repository address follows the current working copy', async () => {
  const repo = load({ url: 'http://svn.example/repo/trunk' })
  assert.equal(repo.remoteRepositoryUrl.value, 'http://svn.example/repo/trunk')
  assert.equal(await repo.openRemoteRepository(), true)
  assert.deepEqual(repo.opened, ['http://svn.example/repo/trunk'])
})

test('nothing opens before the repository address is known', async () => {
  for (const svnInfo of [null, undefined, {}]) {
    const repo = load(svnInfo)
    assert.equal(repo.remoteRepositoryUrl.value, '')
    assert.equal(await repo.openRemoteRepository(), false)
    assert.deepEqual(repo.opened, [])
  }
})

test('a failing system handler is reported to the caller', async () => {
  const repo = load({ url: 'https://fail.example/repo' })
  await assert.rejects(() => repo.openRemoteRepository(), /no handler/)
})
