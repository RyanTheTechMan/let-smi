import { strict as assert } from "node:assert";
import { execFile } from "node:child_process";
import {
  copyFile,
  mkdtemp,
  readdir,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const execute = promisify(execFile);
const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageRoot = resolve(repositoryRoot, "packages/gpu");
const inheritedNpmCli = process.env.npm_execpath;
const windowsNpmCli =
  inheritedNpmCli && basename(inheritedNpmCli).toLowerCase() === "npm-cli.js"
    ? inheritedNpmCli
    : resolve(dirname(process.execPath), "node_modules/npm/bin/npm-cli.js");
const npmCommand =
  process.platform === "win32"
    ? {
        file: process.execPath,
        prefix: [windowsNpmCli],
      }
    : { file: "npm", prefix: [] };
const npmEnvironment = { ...process.env };
for (const name of Object.keys(npmEnvironment)) {
  if (name.toLowerCase().startsWith("npm_config_")) {
    delete npmEnvironment[name];
  }
}
const fixture = await mkdtemp(join(tmpdir(), "let-smi-pack-"));

try {
  const installRoot = join(fixture, "consumer");
  await writeFile(
    join(fixture, "package.json"),
    '{"name":"let-smi-pack-test","private":true}\n',
    "utf8",
  );
  await execute(
    npmCommand.file,
    [
      ...npmCommand.prefix,
      "pack",
      packageRoot,
      "--ignore-scripts",
      "--pack-destination",
      fixture,
    ],
    { cwd: repositoryRoot, env: npmEnvironment },
  );
  const archiveName = (await readdir(fixture)).find((entry) =>
    entry.endsWith(".tgz"),
  );
  assert(archiveName, "npm pack did not produce a tarball");
  await execute(
    npmCommand.file,
    [
      ...npmCommand.prefix,
      "install",
      "--ignore-scripts",
      "--omit=optional",
      "--no-package-lock",
      "--no-audit",
      "--no-fund",
      "--prefix",
      installRoot,
      join(fixture, archiveName),
    ],
    { cwd: fixture, env: npmEnvironment },
  );

  const missing = await runNode(
    installRoot,
    `
      const { GpuMonitor } = require("let-smi");
      GpuMonitor.open().then(
        () => { throw new Error("native addon unexpectedly loaded"); },
        (error) => {
          const message = error instanceof Error ? error.message : String(error);
          if (!message.includes("Unable to load the let-smi native addon") ||
              !message.includes("Reinstall with optional dependencies enabled")) {
            throw error;
          }
          console.log("actionable missing-native error");
        },
      );
    `,
  );
  assert.match(missing.stdout, /actionable missing-native error/u);

  const nativePath = await findHostAddon();
  await copyFile(
    nativePath,
    join(installRoot, "node_modules", "let-smi", basename(nativePath)),
  );
  const esm = await runNode(
    installRoot,
    `
      import { GpuMonitor } from "let-smi";
      const monitor = await GpuMonitor.open();
      try { console.log((await monitor.gpus()).length); }
      finally { await monitor.close(); }
    `,
    ["--input-type=module"],
  );
  const commonJs = await runNode(
    installRoot,
    `
      const { GpuMonitor } = require("let-smi");
      (async () => {
        const monitor = await GpuMonitor.open();
        try { console.log((await monitor.gpus()).length); }
        finally { await monitor.close(); }
      })();
    `,
    [],
  );
  assert.equal(commonJs.stdout.trim(), esm.stdout.trim());
  console.log(
    `Installed ${basename(archiveName)}; ESM and CommonJS each discovered ${esm.stdout.trim()} GPU(s).`,
  );
} finally {
  await rm(fixture, { recursive: true, force: true });
}

async function runNode(
  cwd,
  source,
  argumentsBeforeEval = [],
  env = process.env,
) {
  return execute(process.execPath, [...argumentsBeforeEval, "--eval", source], {
    cwd,
    env,
  });
}

async function findHostAddon() {
  const suffix = (() => {
    if (process.platform === "win32" && process.arch === "x64") {
      return "win32-x64-msvc";
    }
    if (
      process.platform === "darwin" &&
      ["arm64", "x64"].includes(process.arch)
    ) {
      return `darwin-${process.arch}`;
    }
    if (process.platform === "linux" && process.arch === "arm64") {
      return "linux-arm64-gnu";
    }
    if (process.platform === "linux" && process.arch === "x64") {
      return process.report?.getReport?.().header?.glibcVersionRuntime
        ? "linux-x64-gnu"
        : "linux-x64-musl";
    }
    throw new Error(
      `No configured native target for ${process.platform}/${process.arch}`,
    );
  })();
  const path = resolve(packageRoot, `let-smi.${suffix}.node`);
  assert((await stat(path)).isFile(), `build the native addon first: ${path}`);
  return path;
}
