#!/usr/bin/env node
// Version guard: the three manifests must agree, and a `v*` tag must match the
// workspace version. Prints `key=value` lines suitable for `$GITHUB_OUTPUT`.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function fail(message) {
  console.error(`version-guard: ${message}`);
  process.exit(1);
}

const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const cargo = readFileSync(join(root, "src-tauri", "Cargo.toml"), "utf8");
const tauri = JSON.parse(
  readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"),
);

const cargoMatch = /^\s*version\s*=\s*"([^"]+)"/m.exec(cargo);
if (!cargoMatch) fail("Cargo.toml has no [package] version");
const versions = new Set([
  pkg.version,
  cargoMatch[1],
  tauri.version,
]);
if (versions.size !== 1) {
  fail(
    `version drift: package.json=${pkg.version} Cargo.toml=${cargoMatch[1]} tauri.conf.json=${tauri.version}`,
  );
}

const version = pkg.version;
const refName = process.env.GITHUB_REF_NAME ?? "";
const isTag = refName.startsWith("v");
if (isTag && refName !== `v${version}`) {
  fail(`tag ${refName} does not match workspace version v${version}`);
}

// Machine-readable output for `$GITHUB_OUTPUT` (the caller appends `>> $GITHUB_OUTPUT`).
console.log(`version=${version}`);
console.log(`is_tag=${isTag ? "true" : "false"}`);
