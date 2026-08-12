import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const rootManifest = await readJson("package.json");
const packageManifest = await readJson("packages/gpu/package.json");

const targetPackages = new Map([
  ["aarch64-apple-darwin", "let-smi-darwin-arm64"],
  ["x86_64-apple-darwin", "let-smi-darwin-x64"],
  ["aarch64-pc-windows-msvc", "let-smi-win32-arm64-msvc"],
  ["x86_64-pc-windows-msvc", "let-smi-win32-x64-msvc"],
  ["aarch64-unknown-linux-gnu", "let-smi-linux-arm64-gnu"],
  ["x86_64-unknown-linux-gnu", "let-smi-linux-x64-gnu"],
  ["x86_64-unknown-linux-musl", "let-smi-linux-x64-musl"],
]);

assertEqual(packageManifest.name, "let-smi", "public package name");
assertEqual(packageManifest.license, "MIT", "public package license");
assertEqual(
  packageManifest.repository?.url,
  "git+https://github.com/ryanthetechman/let-smi.git",
  "repository URL required by npm trusted publishing",
);
assertEqual(packageManifest.napi?.binaryName, "let-smi", "NAPI binary name");
assertEqual(packageManifest.napi?.packageName, "let-smi", "NAPI package name");
assertEqual(packageManifest.napi?.rootPublisher, "npm", "NAPI root publisher");
assertEqual(
  packageManifest.devDependencies?.["@napi-rs/cli"],
  "3.8.5",
  "pinned NAPI-RS CLI",
);

const targets = requireStringArray(
  packageManifest.napi?.targets,
  "napi.targets",
);
assertArrayEqual(
  [...targets].sort(),
  [...targetPackages.keys()].sort(),
  "NAPI target list",
);

const optionalDependencies = requireRecord(
  packageManifest.optionalDependencies,
  "optionalDependencies",
);
assertArrayEqual(
  Object.keys(optionalDependencies).sort(),
  [...targetPackages.values()].sort(),
  "platform optional package list",
);
for (const packageName of targetPackages.values()) {
  assertEqual(
    optionalDependencies[packageName],
    packageManifest.version,
    `${packageName} version`,
  );
}

const files = new Set(requireStringArray(packageManifest.files, "files"));
for (const requiredFile of ["dist", "native.cjs", "README.md", "LICENSE"]) {
  assert(files.has(requiredFile), `package files must contain ${requiredFile}`);
}

const packageScripts = requireRecord(packageManifest.scripts, "scripts");
for (const prohibitedScript of ["install", "postinstall"]) {
  assert(
    packageScripts[prohibitedScript] === undefined,
    `public package must not define ${prohibitedScript}`,
  );
}
for (const requiredScript of [
  "native:build:debug",
  "native:build:release",
  "native:artifacts",
  "native:create-npm",
  "native:prepublish",
  "prepublishOnly",
]) {
  assert(
    typeof packageScripts[requiredScript] === "string",
    `missing package script ${requiredScript}`,
  );
}

const rootScripts = requireRecord(rootManifest.scripts, "root scripts");
for (const requiredScript of [
  "native:build:debug",
  "native:build:release",
  "native:artifacts",
  "native:create-npm",
  "native:config:check",
  "native:prepublish",
  "native:test-loader",
  "subprocess:check",
]) {
  assert(
    typeof rootScripts[requiredScript] === "string",
    `missing root script ${requiredScript}`,
  );
}

const loader = await readFile(
  resolve(repositoryRoot, "packages/gpu/native.cjs"),
  "utf8",
);
for (const prohibited of [
  /node:child_process/u,
  /require\(["']child_process["']\)/u,
  /\bexec(?:File|Sync)?\s*\(/u,
  /\bspawn(?:Sync)?\s*\(/u,
]) {
  assert(
    !prohibited.test(loader),
    `native loader matched prohibited ${prohibited}`,
  );
}
assert(
  loader.includes("process.report"),
  "native loader must detect libc without an executable",
);

console.log(
  `Packaging configuration is valid for ${targets.length} native targets.`,
);

async function readJson(relativePath) {
  const contents = await readFile(
    resolve(repositoryRoot, relativePath),
    "utf8",
  );
  return JSON.parse(contents);
}

function requireRecord(value, name) {
  assert(
    value !== null && typeof value === "object" && !Array.isArray(value),
    `${name} must be an object`,
  );
  return value;
}

function requireStringArray(value, name) {
  assert(
    Array.isArray(value) && value.every((entry) => typeof entry === "string"),
    `${name} must be an array of strings`,
  );
  return value;
}

function assertArrayEqual(actual, expected, name) {
  assertEqual(JSON.stringify(actual), JSON.stringify(expected), name);
}

function assertEqual(actual, expected, name) {
  assert(
    actual === expected,
    `${name}: expected ${JSON.stringify(expected)}, received ${JSON.stringify(actual)}`,
  );
}

function assert(condition, message) {
  if (!condition) throw new Error(`Packaging validation failed: ${message}`);
}
