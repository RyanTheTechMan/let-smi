import { strict as assert } from "node:assert";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const version = process.argv[2] ?? process.env.RELEASE_VERSION;
if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/u.test(version ?? "")) {
  throw new Error("a valid release version is required");
}

const npmCommand =
  process.platform === "win32"
    ? {
        executable: process.env.ComSpec ?? "cmd.exe",
        prefixArgs: ["/d", "/s", "/c", "npm.cmd"],
      }
    : { executable: "npm", prefixArgs: [] };
const packageSpec = `let-smi@${version}`;
const expectedManifest = JSON.parse(
  await readFile(
    new URL("../packages/gpu/package.json", import.meta.url),
    "utf8",
  ),
);
const nativePackageSpecs = Object.entries(
  expectedManifest.optionalDependencies ?? {},
).map(([name, dependencyVersion]) => `${name}@${dependencyVersion}`);
assert.equal(
  nativePackageSpecs.length,
  6,
  "the release smoke test must cover every native package",
);

await waitForPackages([packageSpec, ...nativePackageSpecs], 20 * 60_000);

const directory = await mkdtemp(join(tmpdir(), "let-smi-registry-"));
await writeFile(
  join(directory, "package.json"),
  `${JSON.stringify({ name: "let-smi-registry-smoke", private: true })}\n`,
  "utf8",
);
runNpm(
  [
    "install",
    "--ignore-scripts",
    "--no-audit",
    "--no-fund",
    "--save-exact",
    packageSpec,
  ],
  { cwd: directory, timeout: 5 * 60_000 },
);

const packageRoot = resolve(directory, "node_modules/let-smi");
const manifest = JSON.parse(
  await readFile(join(packageRoot, "package.json"), "utf8"),
);
assert.equal(manifest.version, version);

const esm = await import(
  pathToFileURL(join(packageRoot, "dist/index.js")).href
);
const monitor = await esm.GpuMonitor.open();
try {
  const gpus = await monitor.gpus();
  assert(Array.isArray(gpus));
} finally {
  await monitor.close();
}

const cjsCheck = spawnSync(
  process.execPath,
  [
    "-e",
    "const { GpuMonitor } = require('let-smi'); GpuMonitor.open().then(async m => { await m.gpus(); await m.close(); });",
  ],
  {
    cwd: directory,
    encoding: "utf8",
    timeout: 30_000,
  },
);
assert.equal(
  cjsCheck.status,
  0,
  `CommonJS registry smoke failed: ${cjsCheck.stderr}`,
);
console.log(
  `Verified ${packageSpec} from npm on ${process.platform}/${process.arch}.`,
);

function runNpm(args, options = {}) {
  const result = spawnSync(
    npmCommand.executable,
    [...npmCommand.prefixArgs, ...args],
    {
      cwd: options.cwd,
      encoding: "utf8",
      env: {
        ...process.env,
        npm_config_registry: "https://registry.npmjs.org/",
      },
      timeout: options.timeout,
      windowsHide: true,
    },
  );
  if (result.error) throw result.error;
  if (!options.allowFailure && result.status !== 0) {
    throw new Error(
      `npm ${args[0]} failed (${String(result.status)}): ${result.stderr}`,
    );
  }
  return result;
}

async function waitForPackages(packageSpecs, timeoutMs) {
  const pending = new Set(packageSpecs);
  const deadline = Date.now() + timeoutMs;
  while (pending.size > 0) {
    for (const pendingPackage of pending) {
      const visible = runNpm(
        ["view", pendingPackage, "version", "--json", "--prefer-online"],
        {
          allowFailure: true,
          timeout: 30_000,
        },
      );
      if (visible.status === 0) pending.delete(pendingPackage);
    }
    if (pending.size === 0) return;
    if (Date.now() >= deadline) {
      throw new Error(
        `npm packages did not become visible within ${String(timeoutMs / 60_000)} minutes: ${[...pending].join(", ")}`,
      );
    }
    console.log(`Waiting for npm publication scan: ${[...pending].join(", ")}`);
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 15_000));
  }
}
