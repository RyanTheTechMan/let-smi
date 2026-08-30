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

const packageSpec = `let-smi@${version}`;
const deadline = Date.now() + 5 * 60_000;
while (true) {
  const visible = runNpm(["view", packageSpec, "version", "--json"], {
    allowFailure: true,
    timeout: 30_000,
  });
  if (visible.status === 0) break;
  if (Date.now() >= deadline) {
    throw new Error(
      `${packageSpec} did not become visible within five minutes`,
    );
  }
  await new Promise((resolvePromise) => setTimeout(resolvePromise, 10_000));
}

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
  const result = spawnSync("npm", args, {
    cwd: options.cwd,
    encoding: "utf8",
    env: {
      ...process.env,
      npm_config_registry: "https://registry.npmjs.org/",
    },
    timeout: options.timeout,
  });
  if (result.error) throw result.error;
  if (!options.allowFailure && result.status !== 0) {
    throw new Error(
      `npm ${args[0]} failed (${String(result.status)}): ${result.stderr}`,
    );
  }
  return result;
}
