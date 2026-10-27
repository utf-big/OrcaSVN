const { normalizeVersion, readVersions } = require('./validate-prerelease.cjs')

function validateRelease(tag, versions) {
  // Three components plus an optional build number, written either as a fourth
  // component or as semver build metadata: v0.6.3 / v0.6.4.20261026 / v0.6.4+20261026
  if (!/^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)([.+](0|[1-9]\d*))?$/.test(tag || '')) {
    throw new Error('Use a stable release tag such as v0.6.3 or v0.6.4+20261026.')
  }
  const version = tag.slice(1)
  for (const [file, actual] of Object.entries(versions)) {
    if (normalizeVersion(actual) !== normalizeVersion(version)) {
      throw new Error(`${file}: expected ${version}, found ${actual}`)
    }
  }
  return version
}

if (require.main === module) {
  const version = validateRelease(process.env.RELEASE_TAG, readVersions())
  console.log(`Validated stable release ${version}`)
}

module.exports = { validateRelease }
