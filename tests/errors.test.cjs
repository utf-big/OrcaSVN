const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const ts = require('typescript')
const vm = require('node:vm')

function load() {
  const compiled = ts.transpileModule(fs.readFileSync('src/utils/errors.ts', 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  const context = {
    exports: {},
    require: name => {
      assert.equal(name, '@/i18n')
      return { __esModule: true, default: { global: { t: key => (key === 'error.hint.svnLocked' ? '先运行清理' : key) } } }
    },
  }
  vm.runInNewContext(compiled, context)
  return context.exports
}

const { errorMessage, parseCommandError, errorCode, errorHintKey, hasErrorCode } = load()

test('a structured command error keeps its message and exposes its code', () => {
  const error = { code: 'svn_locked', message: "svn: E155004: Working copy '/wc' locked." }
  assert.equal(errorCode(error), 'svn_locked')
  assert.equal(hasErrorCode(error, 'svn_locked'), true)
  assert.equal(hasErrorCode(error, 'svn_auth'), false)
  assert.equal(parseCommandError(error).message, "svn: E155004: Working copy '/wc' locked.")
})

test('a code with a known remedy appends the localized hint', () => {
  const message = errorMessage({ code: 'svn_locked', message: 'svn: E155004: locked' })
  assert.equal(message, 'svn: E155004: locked\n先运行清理')
  assert.equal(errorHintKey({ code: 'svn_locked', message: 'x' }), 'error.hint.svnLocked')
})

test('a code without a remedy keeps only the original message', () => {
  assert.equal(errorMessage({ code: 'svn_failed', message: 'boom' }), 'boom')
  assert.equal(errorHintKey({ code: 'svn_failed', message: 'x' }), null)
})

test('legacy string and Error rejections still render', () => {
  assert.equal(errorMessage('plain failure'), 'plain failure')
  assert.equal(errorMessage(new Error('thrown failure')), 'thrown failure')
  assert.equal(errorCode('plain failure'), 'unknown')
  // An empty object must not render as "[object Object]".
  assert.equal(errorMessage({}), '[object Object]')
  assert.equal(parseCommandError({ code: 'svn_timeout' }).code, 'unknown')
})

test('a missing code falls back to unknown but keeps the message', () => {
  const parsed = parseCommandError({ message: 'no code here' })
  assert.equal(parsed.code, 'unknown')
  assert.equal(parsed.message, 'no code here')
})

test('the detail field is optional and normalised', () => {
  assert.equal(parseCommandError({ code: 'svn_failed', message: 'x' }).detail, null)
  assert.equal(parseCommandError({ code: 'svn_failed', message: 'x', detail: 'stderr' }).detail, 'stderr')
})
