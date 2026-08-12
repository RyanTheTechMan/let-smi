import { strict as assert } from "node:assert";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageEntry = pathToFileURL(
  resolve(repositoryRoot, "packages/gpu/dist/index.js"),
);
const { GpuMonitor } = await import(packageEntry.href);

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
    `Public native loader opened on ${diagnostics.platform}/${diagnostics.arch}; discovered ${gpus.length} GPU(s).`,
  );
} finally {
  const firstClose = monitor.close();
  const secondClose = monitor.close();
  assert.equal(firstClose, secondClose, "close() must be idempotent");
  await firstClose;
}
