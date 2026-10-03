import { strict as assert } from "node:assert";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Worker } from "node:worker_threads";
import { GpuMonitor } from "../packages/gpu/dist/index.js";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const required = process.argv.includes("--require-intel-telemetry");
const report = { platform: process.platform, arch: process.arch };

function phase(name) {
  console.error(`Intel hardware phase: ${name}`);
}

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
      assert.equal(typeof metric.available, "boolean");
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
      } else if (group === "temperatures") {
        assert(metric.value >= -100 && metric.value <= 300);
      } else {
        assert(metric.value >= 0);
        if (group === "fan" && field === "percent") assert(metric.value <= 100);
      }
      assert(["direct", "derived", "estimated"].includes(metric.quality));
      if (metric.intervalMs !== undefined) assert(metric.intervalMs >= 0);
    }
  }
}

function checkProcesses(gpu, snapshot, includeProcesses) {
  if (!includeProcesses || !gpu.capabilities.processes) {
    assert.equal(snapshot.processes, undefined, `${gpu.id} process omission`);
  } else {
    assert(Array.isArray(snapshot.processes), `${gpu.id} requested processes`);
    for (const process of snapshot.processes) {
      assert(Number.isSafeInteger(process.pid) && process.pid > 0);
      checkSnapshot({
        memory: { usedBytes: process.memoryUsedBytes },
        utilization: process.utilization,
      });
    }
  }
}

function checkBatch(batch, inventory, includeProcesses) {
  assert.deepEqual(
    batch.gpus.map((gpu) => gpu.deviceId),
    inventory.map((gpu) => gpu.id),
  );
  for (const [index, entry] of batch.gpus.entries()) {
    checkSnapshot(entry.snapshot);
    checkProcesses(inventory[index], entry.snapshot, includeProcesses);
  }
}

phase("open");
const monitor = await GpuMonitor.open();
try {
  phase("discovery and prerequisites");
  const inventory = await monitor.gpus();
  report.devices = inventory.map((gpu) => ({
    identity: gpu.identity,
    capabilities: gpu.capabilities,
  }));
  report.initialDiagnostics = await monitor.diagnostics();
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
    assert.equal(new Set(ids).size, ids.length);
    assert.deepEqual(
      (await monitor.gpus()).map((gpu) => gpu.id),
      ids,
    );
    phase("refresh and scalar telemetry");
    assert.deepEqual(
      (await monitor.refresh()).map((gpu) => gpu.id),
      ids,
    );
    const batch = await monitor.sampleAll({
      windowMs: 1_000,
      includeProcesses: true,
    });
    checkBatch(batch, inventory, true);
    report.snapshots = batch;
    report.diagnostics = await monitor.diagnostics();
    report.intelInfo = await Promise.all(intel.map((gpu) => gpu.intelInfo()));
    assert.deepEqual(
      await Promise.all(intel.map((gpu) => gpu.intelInfo())),
      report.intelInfo,
      "vendor information must not advance engine counters",
    );
    report.unsupportedIntelFields = intel.map((gpu) => ({
      deviceId: gpu.id,
      fields: [
        "temperatures.coreCelsius",
        "temperatures.memoryCelsius",
        "power.drawWatts",
        "power.energyJoules",
        "clocks.memoryMHz",
        "processes",
      ].filter((field) => !gpu.supports(field)),
    }));
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
    for (const metric of successful) {
      if (
        metric.quality === "derived" &&
        metric.definition.includes("engine")
      ) {
        assert(
          metric.intervalMs > 0,
          "Sysman occupancy needs a measured interval",
        );
      }
    }
    phase("pending batch reads and cancellation");
    const controllers = Array.from({ length: 4 }, () => new AbortController());
    const streams = controllers.map((controller, index) =>
      monitor.samplesAll({
        intervalMs: 60_000,
        includeProcesses: index % 2 === 0,
        signal: controller.signal,
      }),
    );
    const initial = await Promise.all(streams.map((stream) => stream.next()));
    initial.forEach((result, index) => {
      assert.equal(result.done, false);
      checkBatch(result.value, inventory, index % 2 === 0);
    });
    report.mixedProcessStreams = initial.map((result, index) => ({
      includeProcesses: index % 2 === 0,
      gpus: result.value.gpus.map((entry) => ({
        deviceId: entry.deviceId,
        processesPresent: entry.snapshot.processes !== undefined,
        processCount: entry.snapshot.processes?.length,
      })),
    }));
    const pending = streams.map((stream) => stream.next());
    const started = performance.now();
    await readFile(resolve(repositoryRoot, "package.json"));
    report.fsReadWhileFourStreamsPendingMs = Math.round(
      performance.now() - started,
    );
    assert(report.fsReadWhileFourStreamsPendingMs < 750);
    const abortStarted = performance.now();
    controllers.forEach((controller) => controller.abort());
    (await Promise.all(pending)).forEach((result) =>
      assert.equal(result.done, true),
    );
    report.abortFourStreamsMs = Math.round(performance.now() - abortStarted);
    assert(report.abortFourStreamsMs < 1_000);
    phase("mixed intervals and early break");
    const timedBatch = monitor.samplesAll({
      intervalMs: 200,
      includeProcesses: true,
    });
    const timedIntel = intel[0].samples({ intervalMs: 500 });
    const timedFirst = await Promise.all([
      timedBatch.next(),
      timedIntel.next(),
    ]);
    checkBatch(timedFirst[0].value, inventory, true);
    checkSnapshot(timedFirst[1].value);
    checkProcesses(intel[0], timedFirst[1].value, false);
    const timedSecond = await Promise.all([
      timedBatch.next(),
      timedIntel.next(),
    ]);
    assert.equal(timedSecond[0].done, false);
    assert.equal(timedSecond[1].done, false);
    checkBatch(timedSecond[0].value, inventory, true);
    checkSnapshot(timedSecond[1].value);
    checkProcesses(intel[0], timedSecond[1].value, false);
    for (let index = 0; index < 2; index += 1) {
      assert(
        timedSecond[index].value.sampledAt > timedFirst[index].value.sampledAt,
      );
    }
    report.streamIntervals = {
      batch: {
        requestedIntervalMs: 200,
        snapshots: timedSecond[0].value,
      },
      intel: {
        requestedIntervalMs: 500,
        snapshot: timedSecond[1].value,
      },
    };
    await Promise.all([timedBatch.return(), timedIntel.return()]);
    for await (const value of monitor.samplesAll({ intervalMs: 100 })) {
      assert.deepEqual(
        value.gpus.map((gpu) => gpu.deviceId),
        ids,
      );
      break;
    }
    phase("worker telemetry and clean exit");
    const worker = new Worker(
      `
      const { parentPort, workerData } = require('node:worker_threads');
      const { GpuMonitor } = require(workerData.entry);
      (async () => { const monitor = await GpuMonitor.open(); try {
        const batch = await monitor.sampleAll({ windowMs: 1_000 }); parentPort.postMessage(batch);
      } finally { await monitor.close(); } })().catch(error => { throw error; });
    `,
      {
        eval: true,
        workerData: {
          entry: resolve(repositoryRoot, "packages/gpu/dist/index.cjs"),
        },
      },
    );
    const workerBatch = new Promise((resolvePromise, reject) => {
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
    const [observedBatch] = await Promise.all([workerBatch, workerExit]);
    checkBatch(observedBatch, inventory, false);
    const afterWorker = await monitor.sampleAll({
      windowMs: 1_000,
      includeProcesses: true,
    });
    checkBatch(afterWorker, inventory, true);
    // A worker closing its providers must not disable the main monitor.
    for (const entry of batch.gpus) {
      const after = afterWorker.gpus.find(
        (gpu) => gpu.deviceId === entry.deviceId,
      );
      const isolated = observedBatch.gpus.find(
        (gpu) => gpu.deviceId === entry.deviceId,
      );
      for (const [group, fields] of Object.entries(entry.snapshot)) {
        if (!fields || typeof fields !== "object" || Array.isArray(fields))
          continue;
        for (const [field, metric] of Object.entries(fields)) {
          if (
            !metric?.available ||
            !["level-zero", "nvml"].includes(metric.source)
          )
            continue;
          for (const snapshot of [after.snapshot, isolated.snapshot]) {
            assert.equal(snapshot[group][field]?.available, true);
            assert.equal(snapshot[group][field].source, metric.source);
          }
        }
      }
    }
    report.workerIsolation = {
      ids: observedBatch.gpus.map((entry) => entry.deviceId),
      telemetryPreservedAfterWorkerClose: true,
    };
    phase("pending-read shutdown and reopen");
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
} catch (error) {
  report.failed = true;
  report.failure = { name: error.name, message: error.message };
  throw error;
} finally {
  phase("close and report");
  await monitor.close();
  console.log(JSON.stringify(report, null, 2));
}
