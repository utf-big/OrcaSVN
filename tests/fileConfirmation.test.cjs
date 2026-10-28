const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')
const vue = require('vue')

function loadStatusAction() {
  const compiled = ts.transpileModule(fs.readFileSync('src/utils/statusAction.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  const context = { exports: {} }
  vm.runInNewContext(compiled, context)
  return context.exports
}

function deferred() {
  let resolve, reject
  const promise = new Promise((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
function harness(status = 'modified', path = 'file.ts') {
  const confirmation = deferred(), dialogs = [], operations = [], opened = []
  const file = { path, status_code: status, prop_status: 'none' }
  const store = vue.reactive({ currentPath: '/wc', statusList: [file], error: null, svnInfo: { url: 'http://svn.example/repo/trunk' }, setError(error) { this.error = error } })
  let refreshes = 0, operation = async () => {}, unmount
  const mocks = {
    '@tauri-apps/plugin-shell': { open: async url => { opened.push(url) } },
    '@/utils/errors': { errorMessage: error => (error && typeof error === 'object' && 'message' in error ? error.message : String(error)) },
    vue: { ...vue, onMounted() {}, onBeforeUnmount(fn) { unmount = fn } },
    'vue-router': { useRouter: () => ({}) },
    'vue-i18n': { useI18n: () => ({ t: key => key }) },
    '@/stores/workspace': { useWorkspaceStore: () => store },
    '@/api/svn': {
      svnRevert: async (...args) => { operations.push({ type: 'revert', args }); await operation() },
      svnDelete: async (...args) => { operations.push({ type: 'scheduleDelete', args }); await operation() },
      svnResolve: async (...args) => { operations.push({ type: 'resolve', args }); await operation() },
      svnRepairNames: async (...args) => { operations.push({ type: 'repairNames', args }); return 1 },
      svnDeleteDuplicates: async (...args) => { operations.push({ type: 'deleteDuplicates', args }); return { success: true, output: 'D', error: null } },
      deleteUnversioned: async (...args) => { operations.push({ type: 'delete', args }); await operation() },
    },
    'element-plus/es/components/message/index': {},
    'element-plus/es/components/message-box/index': { ElMessageBox: { confirm: (...args) => { dialogs.push(args); return confirmation.promise } } },
    '@/composables/useSvnStatus': {},
    '@/composables/useWorkspace': { useWorkspace: () => ({ refreshStatusAfterMutation: async () => { refreshes++; return true } }) },
    '@/components/VirtualViewport.vue': {},
    '@/components/DiffViewer.vue': {},
    '@/components/GlassLoading.vue': {},
    '@/components/SwitchRepositoryDialog.vue': {},
    '@/components/MergeDialog.vue': {},
    '@/utils/svnInfoCache': { invalidateSvnInfoMetadata() {} },
    '@/utils/clipboard': {},
    '@/utils/statusAction': loadStatusAction(),
  }
  const script = fs.readFileSync('src/views/WorkspaceView.vue', 'utf8').match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1]
  const code = ts.transpileModule(script + '\nexports.actions = { handleFileAction, handleScheduleDelete, handleResolve, handleRepairNames, handleDeleteDuplicates, fileActionLabel, pendingFileAction, canScheduleDelete, canResolve, canRepairName, canDeleteDuplicate };', { compilerOptions: { module: ts.ModuleKind.CommonJS } }).outputText
  const context = { exports: {}, document: { removeEventListener() {} }, require: name => { assert.ok(name in mocks, name); return mocks[name] } }
  vm.runInNewContext(code, context)
  return { ...context.exports.actions, file, store, confirmation, dialogs, operations,
    refreshes: () => refreshes, setOperation: fn => { operation = fn }, unmount: () => unmount() }
}

test('revert waits for explicit confirmation and displays the target as escaped text', async () => {
  const h = harness('modified', '<img src=x onerror=alert(1)>.ts')
  const pending = h.handleFileAction(h.file)
  assert.equal(h.operations.length, 0)
  assert.equal(h.dialogs.length, 1)
  const [message, title, options] = h.dialogs[0]
  assert.equal(title, 'common.revert')
  assert.equal(message.children[0].children, 'workspace.revertFileConfirm')
  assert.equal(message.children[1].children, '/wc/<img src=x onerror=alert(1)>.ts')
  assert.equal(message.children[1].props.innerHTML, undefined)
  assert.equal(options.closeOnClickModal, false)
  assert.equal(options.autofocus, false)
  h.confirmation.resolve('confirm')
  await pending
  assert.equal(h.operations[0].type, 'revert')
  assert.equal(h.operations[0].args[0], '/wc')
  assert.equal(Array.from(h.operations[0].args[1])[0], h.file.path)
  assert.equal(h.refreshes(), 1)
  assert.equal(h.pendingFileAction.value, null)
})

test('unversioned targets use Delete labels and the permanent deletion warning', async () => {
  const h = harness('unversioned', 'new-directory')
  assert.equal(h.fileActionLabel(h.file), 'common.delete')
  const pending = h.handleFileAction(h.file)
  assert.equal(h.dialogs[0][0].children[0].children, 'workspace.deleteUnversionedConfirm')
  assert.equal(h.dialogs[0][2].confirmButtonText, 'common.delete')
  h.confirmation.resolve('confirm')
  await pending
  assert.equal(h.operations[0].type, 'delete')
})

test('cancel and close leave files and error state unchanged', async () => {
  for (const action of ['cancel', 'close']) {
    const h = harness()
    const pending = h.handleFileAction(h.file)
    h.confirmation.reject(action)
    await pending
    assert.equal(h.operations.length, 0)
    assert.equal(h.refreshes(), 0)
    assert.equal(h.store.error, null)
    assert.equal(h.pendingFileAction.value, null)
  }
})

test('a workspace switch, even away and back, invalidates the confirmation', async () => {
  const h = harness()
  const pending = h.handleFileAction(h.file)
  h.store.currentPath = '/another'
  h.store.currentPath = '/wc'
  h.confirmation.resolve('confirm')
  await pending
  assert.equal(h.operations.length, 0)
})

test('removed or changed SVN status requires a new confirmation', async () => {
  for (const change of [h => { h.store.statusList = [] }, h => { h.store.statusList[0].status_code = 'added' }]) {
    const h = harness('unversioned')
    const pending = h.handleFileAction(h.file)
    change(h)
    h.confirmation.resolve('confirm')
    await pending
    assert.equal(h.operations.length, 0)
  }
})

test('duplicate clicks cannot open multiple dialogs or execute the operation twice', async () => {
  const h = harness(), execution = deferred()
  h.setOperation(() => execution.promise)
  const first = h.handleFileAction(h.file)
  await h.handleFileAction(h.file)
  assert.equal(h.dialogs.length, 1)
  h.confirmation.resolve('confirm')
  for (let i = 0; i < 5; i++) await Promise.resolve()
  await h.handleFileAction(h.file)
  assert.equal(h.operations.length, 1)
  execution.resolve()
  await first
  assert.equal(h.pendingFileAction.value, null)
})

test('operation failures retain their error and unlock file actions', async () => {
  const h = harness()
  h.setOperation(async () => { throw new Error('SVN failed') })
  const pending = h.handleFileAction(h.file)
  h.confirmation.resolve('confirm')
  await pending
  // errorMessage() reports the message without the Error: prefix.
  assert.equal(h.store.error, 'SVN failed')
  assert.equal(h.refreshes(), 0)
  assert.equal(h.pendingFileAction.value, null)
})

test('unmounting the view invalidates a pending confirmation', async () => {
  const h = harness()
  const pending = h.handleFileAction(h.file)
  h.unmount()
  h.confirmation.resolve('confirm')
  await pending
  assert.equal(h.operations.length, 0)
})


test('unverified cached status cannot start or confirm a destructive file action', async () => {
  const h = harness()
  h.store.statusIsStale = true
  await h.handleFileAction(h.file)
  assert.equal(h.dialogs.length, 0)
  h.store.statusIsStale = false
  const pending = h.handleFileAction(h.file)
  h.store.statusIsStale = true
  h.confirmation.resolve('confirm')
  await pending
  assert.equal(h.operations.length, 0)
})

test('a missing file is scheduled for deletion only after confirmation', async () => {
  const h = harness('missing', 'src/gone.txt')
  assert.equal(h.canScheduleDelete(h.file), true)
  const pending = h.handleScheduleDelete(h.file)
  assert.equal(h.operations.length, 0)
  const [message, title, options] = h.dialogs[0]
  assert.equal(title, 'workspace.scheduleDelete')
  assert.equal(message.children[0].children, 'workspace.scheduleDeleteConfirm')
  assert.equal(message.children[1].children, '/wc/src/gone.txt')
  assert.equal(options.confirmButtonText, 'workspace.scheduleDelete')
  h.confirmation.resolve('confirm')
  await pending
  assert.equal(h.operations[0].type, 'scheduleDelete')
  assert.deepEqual(Array.from(h.operations[0].args[1]), ['src/gone.txt'])
  assert.equal(h.refreshes(), 1)
})

test('scheduling deletion is refused for a file that is not missing', async () => {
  for (const status of ['modified', 'added', 'unversioned']) {
    const h = harness(status)
    await h.handleScheduleDelete(h.file)
    assert.equal(h.dialogs.length, 0, status)
    assert.equal(h.operations.length, 0, status)
  }
})

test('a conflicted file is resolved with the chosen strategy', async () => {
  for (const strategy of ['working', 'mine-full', 'theirs-full']) {
    const h = harness('conflicted', 'src/conflict.txt')
    assert.equal(h.canResolve(h.file), true)
    const pending = h.handleResolve(h.file, strategy)
    assert.equal(h.operations.length, 0)
    assert.equal(h.dialogs[0][1], 'workspace.resolveFile')
    assert.equal(h.dialogs[0][0].children[0].children, 'workspace.resolveFileConfirm')
    h.confirmation.resolve('confirm')
    await pending
    assert.equal(h.operations[0].type, 'resolve')
    assert.deepEqual(Array.from(h.operations[0].args[1]), ['src/conflict.txt'])
    assert.equal(h.operations[0].args[2], strategy)
    assert.equal(h.refreshes(), 1)
  }
})

test('resolving is refused for a file without a conflict', async () => {
  const h = harness('modified', 'src/a.txt')
  await h.handleResolve(h.file, 'working')
  assert.equal(h.dialogs.length, 0)
  assert.equal(h.operations.length, 0)
})

test('cancelling a resolution keeps the file and its error state untouched', async () => {
  const h = harness('conflicted', 'src/conflict.txt')
  const pending = h.handleResolve(h.file, 'theirs-full')
  h.confirmation.reject('cancel')
  await pending
  assert.equal(h.operations.length, 0)
  assert.equal(h.store.error, null)
  assert.equal(h.pendingFileAction.value, null)
})

test('a name-mismatch entry is repaired by renaming instead of reverting', async () => {
  const h = harness('name_mismatch', 'src/ポ.txt')
  assert.equal(h.canRepairName(h.file), true)
  const pending = h.handleRepairNames([h.file.path])
  assert.equal(h.operations.length, 0)
  const [message, title, options] = h.dialogs[0]
  assert.equal(title, 'workspace.repairName')
  assert.equal(message.children[0].children, 'workspace.repairNameConfirm')
  // A single target shows its absolute path like the other confirmations.
  assert.equal(message.children[1].children, '/wc/src/ポ.txt')
  assert.equal(options.confirmButtonType, 'primary')
  h.confirmation.resolve('confirm')
  await pending
  assert.equal(h.operations[0].type, 'repairNames')
  assert.deepEqual(Array.from(h.operations[0].args[1]), ['src/ポ.txt'])
  assert.equal(h.refreshes(), 1)
})

test('repairing every mismatched name sends them in one call without a path list', async () => {
  const h = harness('name_mismatch', 'a.txt')
  h.store.statusList.push({ path: 'b.txt', status_code: 'name_mismatch', prop_status: 'none' })
  const pending = h.handleRepairNames(['a.txt', 'b.txt'])
  const [message] = h.dialogs[0]
  assert.equal(message.children.length, 1, 'only the count is shown for several targets')
  h.confirmation.resolve('confirm')
  await pending
  assert.deepEqual(Array.from(h.operations[0].args[1]), ['a.txt', 'b.txt'])
})

test('an unverified cached list blocks renaming files', async () => {
  const h = harness('name_mismatch', 'src/ポ.txt')
  h.store.statusIsStale = true
  await h.handleRepairNames(['src/ポ.txt'])
  assert.equal(h.dialogs.length, 0)
  assert.equal(h.operations.length, 0)
})

test('a repository duplicate is deleted from version control instead of renamed', async () => {
  const h = harness('name_duplicate', 'src/ポ.xlsx')
  assert.equal(h.canDeleteDuplicate(h.file), true)
  assert.equal(h.canRepairName(h.file), false)
  const pending = h.handleDeleteDuplicates([h.file.path])
  assert.equal(h.operations.length, 0)
  const [message, title, options] = h.dialogs[0]
  assert.equal(title, 'workspace.deleteDuplicate')
  assert.equal(message.children[0].children, 'workspace.deleteDuplicateConfirm')
  assert.equal(options.confirmButtonType, 'danger')
  h.confirmation.resolve('confirm')
  await pending
  assert.equal(h.operations[0].type, 'deleteDuplicates')
  assert.deepEqual(Array.from(h.operations[0].args[1]), ['src/ポ.xlsx'])
  assert.equal(h.refreshes(), 1)
})

test('deleting duplicates keeps the count-only message for several targets', async () => {
  const h = harness('name_duplicate', 'a.xlsx')
  const pending = h.handleDeleteDuplicates(['a.xlsx', 'b.xlsx'])
  assert.equal(h.dialogs[0][0].children.length, 1)
  h.confirmation.resolve('confirm')
  await pending
  assert.deepEqual(Array.from(h.operations[0].args[1]), ['a.xlsx', 'b.xlsx'])
})
