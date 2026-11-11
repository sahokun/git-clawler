#!/usr/bin/env node
import { readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const version = process.argv[2]
const SEMVER_RE =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/

if (!version || !SEMVER_RE.test(version)) {
  console.error('Usage: node scripts/bump-version.mjs <MAJOR.MINOR.PATCH[-prerelease][+build]>')
  process.exit(1)
}

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const cargoTomlPath = resolve(repoRoot, 'Cargo.toml')
const packageJsonPath = resolve(repoRoot, 'crates/gui/package.json')
const packageLockPath = resolve(repoRoot, 'crates/gui/package-lock.json')
const tauriConfPath = resolve(repoRoot, 'crates/gui/tauri.conf.json')

function prepareCargoToml(path, input, nextVersion) {
  const workspacePackageSection =
    /(\[workspace\.package\][\s\S]*?^version\s*=\s*")([^"]+)(")/m
  if (!workspacePackageSection.test(input)) {
    throw new Error('Cargo.toml: [workspace.package].version was not found')
  }
  return input.replace(workspacePackageSection, `$1${nextVersion}$3`)
}

function prepareJsonVersion(path, input, nextVersion) {
  const json = JSON.parse(input)
  if (typeof json.version !== 'string') {
    throw new Error(`${path}: top-level version must be a string`)
  }
  json.version = nextVersion
  return `${JSON.stringify(json, null, 2)}\n`
}

function replaceExactlyOnce(path, input, regex, nextVersion, label) {
  const matches = [...input.matchAll(regex)]
  if (matches.length !== 1) {
    throw new Error(`${path}: expected exactly one ${label}, found ${matches.length}`)
  }
  return input.replace(regex, `$1${nextVersion}$3`)
}

function preparePackageLockVersion(path, input, nextVersion) {
  const topLevelVersionRe = /^(\s{2}"version": ")([^"]+)(",$)/gm
  const rootPackageVersionRe =
    /(^\s{4}"": \{\r?\n\s{6}"name": "[^"]+",\r?\n\s{6}"version": ")([^"]+)(",$)/gm

  let output = replaceExactlyOnce(path, input, topLevelVersionRe, nextVersion, 'top-level version')
  output = replaceExactlyOnce(path, output, rootPackageVersionRe, nextVersion, 'packages[""].version')

  const json = JSON.parse(output)
  if (json.version !== nextVersion) {
    throw new Error(`${path}: top-level version was not updated`)
  }
  if (
    !json.packages ||
    typeof json.packages !== 'object' ||
    !json.packages[''] ||
    typeof json.packages[''] !== 'object' ||
    json.packages[''].version !== nextVersion
  ) {
    throw new Error(`${path}: packages[""].version was not updated`)
  }

  return output
}

const [cargoInput, packageInput, tauriInput, packageLockInput] = await Promise.all([
  readFile(cargoTomlPath, 'utf8'),
  readFile(packageJsonPath, 'utf8'),
  readFile(tauriConfPath, 'utf8'),
  readFile(packageLockPath, 'utf8'),
])

const cargoOutput = prepareCargoToml(cargoTomlPath, cargoInput, version)
const packageOutput = prepareJsonVersion(packageJsonPath, packageInput, version)
const tauriOutput = prepareJsonVersion(tauriConfPath, tauriInput, version)
const packageLockOutput = preparePackageLockVersion(packageLockPath, packageLockInput, version)

await Promise.all([
  writeFile(cargoTomlPath, cargoOutput),
  writeFile(packageJsonPath, packageOutput),
  writeFile(tauriConfPath, tauriOutput),
  writeFile(packageLockPath, packageLockOutput),
])

console.log(`Synchronized version to ${version}`)
