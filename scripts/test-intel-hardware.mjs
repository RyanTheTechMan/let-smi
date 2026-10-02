import { strict as assert } from "node:assert";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";
import { GpuMonitor } from "../packages/gpu/dist/index.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const required = process.argv.includes("--require-intel-telemetry");
const report = { platform: process.platform, arch: process.arch };

function checkSnapshot(snapshot) {
  for (const [group, fields] of Object.entries(snapshot)) {
    if (fields === null || typeof fields !== "object" || Array.isArray(fields))
      continue;
    for (const [field, metric] of Object.entries(fields)) {
      if (
        typeof metric !== "object" ||
        metric === null ||
        !("available" in metric)
      )
        continue;
      if (!metric.available) {
        assert.equal(typeof metric.reason, "string");
        assert(!("value" in metric));
        continue;
      }
      assert(Number.isFinite(metric.value), `${group}.${field}`);
      assert(Number.isFinite(metric.sampledAt));
      assert.equal(typeof metric.source, "string");
      if (group === "utilization") {
        assert(metric.value >= 0 && metric.value <= 100);
        assert.equal(typeof metric.definition, "string");
      } else if (group !== "temperatures") assert(metric.value >= 0);
      if (metric.intervalMs !== undefined) assert(metric.intervalMs >= 0);
    }
  }
}

const monitor = await GpuMonitor.open();
try {
  const inventory = await monitor.gpus();
  const intel = inventory.filter((gpu) => gpu.vendor === "intel");
  if (!["win32", "linux"].includes(process.platform) || intel.length === 0) {
    if (required)
      throw new Error(
        "Intel release validation requires a Windows or Linux Intel host",
      );
    report.skipped = true;
    report.reason = "requires Windows/Linux with an Intel GPU";
  } else {
    const ids = inventory.map((gpu) => gpu.id);
    assert.deepEqual(
      (await monitor.refresh()).map((gpu) => gpu.id),
      ids,
    );
    const batch = await monitor.sampleAll({
      windowMs: 1_000,
      includeProcesses: true,
    });
    assert.deepEqual(
      batch.gpus.map((gpu) => gpu.deviceId),
      ids,
    );
    batch.gpus.forEach((entry) => checkSnapshot(entry.snapshot));
    report.devices = inventory.map((gpu) => ({
      identity: gpu.identity,
      capabilities: gpu.capabilities,
    }));
    report.snapshots = batch;
    report.diagnostics = await monitor.diagnostics();
    report.intelInfo = await Promise.all(intel.map((gpu) => gpu.intelInfo()));
    const successful = batch.gpus
      .filter((entry) => intel.some((gpu) => gpu.id === entry.deviceId))
      .flatMap((entry) =>
        Object.values(entry.snapshot)
          .filter(
            (value) =>
              value && typeof value === "object" && !Array.isArray(value),
          )
          .flatMap((fields) => Object.values(fields))
          .filter(
            (metric) => metric?.available && metric.source === "level-zero",
          ),
      );
    if (successful.length === 0 && required)
      throw new Error(
        "No successful Intel Sysman readings; a skip is not release validation",
      );
    if (process.platform === "linux" && required) {
      assert(
        batch.gpus.some(
          (entry) =>
            intel.some((gpu) => gpu.id === entry.deviceId) &&
            entry.snapshot.utilization.overall.available &&
            entry.snapshot.utilization.overall.source === "level-zero",
        ),
        "Linux release validation requires measured Intel utilization",
      );
    }
    report.intelTelemetryAvailable = successful.length > 0;
    const controllers = Array.from({ length: 4 }, () => new AbortController());
    const streams = controllers.map((controller, index) =>
      monitor.samplesAll({
        intervalMs: 60_000,
        includeProcesses: index % 2 === 0,
        signal: controller.signal,
      }),
    );
    const initial = await Promise.all(streams.map((stream) => stream.next()));
    initial.forEach((result) => {
      assert.equal(result.done, false);
      result.value.gpus.forEach((entry) => checkSnapshot(entry.snapshot));
    });
    const pending = streams.map((stream) => stream.next());
    const started = performance.now();
    await readFile(resolve(repositoryRoot, "package.json"));
    report.fsReadWhileFourStreamsPendingMs = Math.round(
      performance.now() - started,
    );
    assert(report.fsReadWhileFourStreamsPendingMs < 750);
    controllers.forEach((controller) => controller.abort());
    (await Promise.all(pending)).forEach((result) =>
      assert.equal(result.done, true),
    );
    for await (const value of monitor.samplesAll({ intervalMs: 100 })) {
      assert.deepEqual(
        value.gpus.map((gpu) => gpu.deviceId),
        ids,
      );
      break;
    }
    const worker = new Worker(
      `
      const { parentPort, workerData } = require('node:worker_threads');
      const { GpuMonitor } = require(workerData.entry);
      (async () => { const monitor = await GpuMonitor.open(); try {
        const batch = await monitor.sampleAll(); parentPort.postMessage(batch.gpus.map(gpu => gpu.deviceId));
      } finally { await monitor.close(); } })().catch(error => { throw error; });
    `,
      {
        eval: true,
        workerData: {
          entry: resolve(repositoryRoot, "packages/gpu/dist/index.cjs"),
        },
      },
    );
    const workerIds = new Promise((resolvePromise, reject) => {
      worker.once("message", resolvePromise);
      worker.once("error", reject);
    });
    const workerExit = new Promise((resolvePromise, reject) => {
      worker.once("error", reject);
      worker.once("exit", (code) =>
        code === 0
          ? resolvePromise()
          : reject(new Error(`worker exit ${code}`)),
      );
    });
    const [observedIds] = await Promise.all([workerIds, workerExit]);
    assert.deepEqual(observedIds, ids);
    const closingStream = monitor.samplesAll({ intervalMs: 60_000 });
    await closingStream.next();
    const closingNext = closingStream.next();
    const closeStarted = performance.now();
    await monitor.close();
    report.closeMs = Math.round(performance.now() - closeStarted);
    assert(report.closeMs < 3_000);
    assert.equal((await closingNext).done, true);
    await monitor.close();
    await assert.rejects(monitor.sampleAll());
    const reopened = await GpuMonitor.open();
    try {
      assert.deepEqual(
        (await reopened.gpus()).map((gpu) => gpu.id),
        ids,
      );
    } finally {
      await reopened.close();
    }
  }
} finally {
  await monitor.close();
}
console.log(JSON.stringify(report, null, 2));
