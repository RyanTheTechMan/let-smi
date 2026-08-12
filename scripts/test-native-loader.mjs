import { strict as assert } from "node:assert";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const esmEntry = pathToFileURL(
  resolve(repositoryRoot, "packages/gpu/dist/index.js"),
);
const cjsEntry = resolve(repositoryRoot, "packages/gpu/dist/index.cjs");
const esm = await import(esmEntry.href);
const cjs = createRequire(import.meta.url)(cjsEntry);

const esmIds = await smoke(esm.GpuMonitor, "ESM");
const cjsIds = await smoke(cjs.GpuMonitor, "CommonJS");
assert.deepEqual(
  cjsIds,
  esmIds,
  "ESM and CommonJS must discover the same GPUs",
);

async function smoke(GpuMonitor, moduleKind) {
  assert.equal(
    typeof GpuMonitor?.open,
    "function",
    `${moduleKind} public export`,
  );
  const monitor = await GpuMonitor.open({ enableApplePrivateTelemetry: false });
  try {
    const gpus = await monitor.gpus();
    assert(Array.isArray(gpus), "gpus() must resolve to an array");
    assert.equal(
      new Set(gpus.map((gpu) => gpu.id)).size,
      gpus.length,
      "GPU ids must be unique",
    );

    const diagnostics = await monitor.diagnostics();
    assert.equal(typeof diagnostics.platform, "string");
    assert.equal(typeof diagnostics.arch, "string");
    assert(Array.isArray(diagnostics.providers));
    assert(Array.isArray(diagnostics.warnings));

    console.log(
      `${moduleKind} public loader opened on ${diagnostics.platform}/${diagnostics.arch}; discovered ${gpus.length} GPU(s).`,
    );
    return gpus.map((gpu) => gpu.id);
  } finally {
    const firstClose = monitor.close();
    const secondClose = monitor.close();
    assert.equal(firstClose, secondClose, "close() must be idempotent");
    await firstClose;
  }
}
