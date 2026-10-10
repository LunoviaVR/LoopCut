#!/usr/bin/env node
// Bumps the LoopCut application version everywhere it is recorded:
//   - package.json                 "version" (tauri.conf.json reads it through "version": "../package.json")
//   - package-lock.json            root "version" and packages[""].version
//   - Cargo.toml                   [workspace.package] version (inherited by every workspace crate), and the
//                                  version of each internal path dependency in [workspace.dependencies]
//                                  (`{ path = "crates/...", version = "..." }`, added when missing)
//
// Only the version lines are rewritten, so the rest of each file keeps its formatting. Each rewritten
// file is re-parsed afterwards to confirm the new version is in place.
//
// Usage: node scripts/bump-version.mjs <major.minor.patch>
//
// Release versions are plain MAJOR.MINOR.PATCH. The new version must be greater than the current one,
// because the updater refuses downgrades and rollbacks are published as a higher version.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SEMVER = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
// `name = { path = "crates/...", ...rest }` inside [workspace.dependencies].
const INTERNAL_DEP = /^(\s*[\w-]+\s*=\s*\{\s*path\s*=\s*"crates\/[^"]+")(.*?)\s*\}(.*)$/;

function parseVersion(version) {
  const match = SEMVER.exec(version);
  if (!match) return null;
  return match.slice(1).map(Number);
}

/** Returns a negative, zero or positive number as `a` is lower, equal or higher than `b`. */
function compareVersions(a, b) {
  for (let i = 0; i < 3; i += 1) {
    if (a[i] !== b[i]) return a[i] - b[i];
  }
  return 0;
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** Replaces exactly one match of `pattern` in `text`, failing loudly otherwise. */
function replaceOnce(text, pattern, replacement, label) {
  const global = new RegExp(
    pattern.source,
    pattern.flags.includes("g") ? pattern.flags : `${pattern.flags}g`,
  );
  const count = [...text.matchAll(global)].length;
  if (count !== 1) throw new Error(`${label}: expected exactly one version line, found ${count}`);
  return text.replace(pattern, replacement);
}

/** Returns the [start, end) line range of a TOML table, or null when the header is absent. */
function tableRange(lines, header) {
  const start = lines.findIndex((line) => line.trim() === header);
  if (start < 0) return null;
  let end = lines.length;
  for (let i = start + 1; i < lines.length; i += 1) {
    if (/^\s*\[/.test(lines[i])) {
      end = i;
      break;
    }
  }
  return { start, end };
}

/**
 * Rewrites Cargo.toml. Sets the `version` line of `[workspace.package]`, and sets (or adds) the `version` of every
 * internal path dependency in `[workspace.dependencies]`. Returns the new text and the number of internal
 * dependencies updated.
 */
function bumpCargoToml(text, next) {
  const lines = text.split("\n");

  const pkg = tableRange(lines, "[workspace.package]");
  if (!pkg) throw new Error("Cargo.toml: no [workspace.package] table");
  const versionLines = [];
  for (let i = pkg.start + 1; i < pkg.end; i += 1) {
    if (/^version\s*=\s*"[^"]*"\s*$/.test(lines[i])) versionLines.push(i);
  }
  if (versionLines.length !== 1) {
    throw new Error(
      `Cargo.toml: expected one version line in [workspace.package], found ${versionLines.length}`,
    );
  }
  const index = versionLines[0];
  lines[index] = lines[index].replace(/"[^"]*"/, `"${next}"`);

  let internal = 0;
  const deps = tableRange(lines, "[workspace.dependencies]");
  for (let i = deps ? deps.start + 1 : 0; deps && i < deps.end; i += 1) {
    const match = INTERNAL_DEP.exec(lines[i]);
    if (!match) continue;
    const [, head, attrs, tail] = match;
    const versioned = /\bversion\s*=\s*"[^"]*"/.test(attrs);
    const nextAttrs = versioned
      ? attrs.replace(/\bversion\s*=\s*"[^"]*"/, `version = "${next}"`)
      : `${attrs}, version = "${next}"`;
    lines[i] = `${head}${nextAttrs} }${tail}`;
    internal += 1;
  }
  return { text: lines.join("\n"), internal };
}

/** Rewrites the root version and packages[""].version of package-lock.json without reformatting it. */
function bumpPackageLock(text, current, next) {
  const rootVersion = `"version": "${current}"`;
  const packagesAt = text.indexOf('"packages": {');
  if (packagesAt < 0) throw new Error('package-lock.json: no "packages" object');
  const rootEntryAt = text.indexOf('"": {', packagesAt);
  if (rootEntryAt < 0) throw new Error('package-lock.json: no packages[""] entry');

  const topLevelAt = text.indexOf(rootVersion);
  if (topLevelAt < 0 || topLevelAt > packagesAt) {
    throw new Error(
      `package-lock.json: top-level "version": "${current}" not found before "packages"`,
    );
  }
  const rootVersionAt = text.indexOf(rootVersion, rootEntryAt);
  if (rootVersionAt < 0) throw new Error('package-lock.json: no version inside packages[""]');

  const replaceAt = (source, index, from, to) =>
    source.slice(0, index) + to + source.slice(index + from.length);
  // Replace the later occurrence first so the earlier index stays valid.
  let out = replaceAt(text, rootVersionAt, rootVersion, `"version": "${next}"`);
  out = replaceAt(out, topLevelAt, rootVersion, `"version": "${next}"`);
  return out;
}

function main() {
  const next = process.argv[2];
  if (process.argv.length !== 3 || next === undefined) {
    console.error("usage: node scripts/bump-version.mjs <major.minor.patch>");
    process.exit(2);
  }
  const nextParts = parseVersion(next);
  if (!nextParts) {
    console.error(`bump-version: "${next}" is not a plain MAJOR.MINOR.PATCH semver version`);
    process.exit(1);
  }

  const packagePath = join(root, "package.json");
  const lockPath = join(root, "package-lock.json");
  const cargoPath = join(root, "Cargo.toml");

  const pkgText = readFileSync(packagePath, "utf8");
  const current = JSON.parse(pkgText).version;
  const currentParts = parseVersion(current);
  if (!currentParts) {
    console.error(`bump-version: package.json version "${current}" is not MAJOR.MINOR.PATCH`);
    process.exit(1);
  }
  if (compareVersions(nextParts, currentParts) <= 0) {
    console.error(
      `bump-version: ${next} must be greater than the current version ${current} ` +
        "(the updater refuses downgrades; roll back by publishing a higher version)",
    );
    process.exit(1);
  }

  const lockText = readFileSync(lockPath, "utf8");
  const lock = JSON.parse(lockText);
  if (lock.version !== current || lock.packages?.[""]?.version !== current) {
    console.error(
      "bump-version: package-lock.json does not match package.json; run npm install first",
    );
    process.exit(1);
  }
  const cargoText = readFileSync(cargoPath, "utf8");

  // Build all three new texts before writing anything, so a failure leaves the tree untouched.
  const newPkg = replaceOnce(
    pkgText,
    new RegExp(`^(\\s*"version":\\s*)"${escapeRegExp(current)}"`, "m"),
    `$1"${next}"`,
    "package.json",
  );
  const newLock = bumpPackageLock(lockText, current, next);
  const cargo = bumpCargoToml(cargoText, next);

  // Verify the results parse and carry the new version.
  if (JSON.parse(newPkg).version !== next) throw new Error("package.json: version was not updated");
  const parsedLock = JSON.parse(newLock);
  if (parsedLock.version !== next || parsedLock.packages?.[""]?.version !== next) {
    throw new Error("package-lock.json: version was not updated");
  }
  if (!cargo.text.includes(`version = "${next}"`)) {
    throw new Error("Cargo.toml: version was not updated");
  }

  writeFileSync(packagePath, newPkg);
  writeFileSync(lockPath, newLock);
  writeFileSync(cargoPath, cargo.text);

  console.log(`bump-version: ${current} -> ${next}`);
  console.log("  updated package.json, package-lock.json");
  console.log(
    `  updated Cargo.toml: [workspace.package] and ${cargo.internal} internal path dependencies`,
  );
  if (cargo.internal === 0) {
    console.warn("  warning: no internal path dependencies found in [workspace.dependencies]");
  }
  console.log("");
  console.log("Next steps:");
  console.log(
    "  1. cargo update --workspace        # refresh the workspace crate versions in Cargo.lock",
  );
  console.log("  2. npm run lint && cargo test --workspace   # sanity check");
  console.log(`  3. git add -A && git commit -m "Release v${next}"`);
  console.log(`  4. git tag -a v${next} -m "LoopCut v${next}"`);
  console.log(
    `  5. git push origin main v${next}   # release.yml builds a draft release for the tag`,
  );
}

main();
