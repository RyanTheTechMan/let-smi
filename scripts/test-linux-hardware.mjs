import { strict as assert } from "node:assert";
import { readFile } from "node:fs/promises";
import { arch, release } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { Worker } from "node:worker_threads";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageEntry = pathToFileURL(
  resolve(repositoryRoot, "packages/gpu/dist/index.js"),
).href;
const fixturePath = fileURLToPath(import.meta.url);
const { GpuMonitor } = await import(packageEntry);

async function withDeadline(promise, milliseconds, label) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(
          () =>
            reject(new Error(`${label} exceeded ${String(milliseconds)} ms`)),
          milliseconds,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function provider(diagnostics, id) {
  return diagnostics.providers.find((candidate) => candidate.id === id);
}

async function checkPrerequisites() {
  if (process.platform !== "linux" || process.arch !== "x64") {
    return "requires Linux x64";
  }
  const monitor = await GpuMonitor.open();
  try {
    const gpus = await monitor.gpus();
    const diagnostics = await monitor.diagnostics();
    if (!gpus.some((gpu) => gpu.vendor === "nvidia")) {
      return "requires at least one NVIDIA GPU";
    }
    if (!gpus.some((gpu) => gpu.vendor === "intel")) {
      return "requires at least one Intel GPU";
    }
    if (provider(diagnostics, "linux-sysfs")?.loaded !== true) {
      return "requires functional Linux sysfs discovery";
    }
    if (provider(diagnostics, "nvml")?.loaded !== true) {
      return "requires functional NVIDIA NVML";
    }
    return undefined;
  } finally {
    await monitor.close();
  }
}

function assertPciIdentity(gpu, expectedVendorId) {
  const pci = gpu.identity.pci;
  assert(pci, `${gpu.vendor} PCI identity missing`);
  assert.match(
    pci.address ?? "",
    /^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$/u,
    `${gpu.vendor} PCI address must be normalized`,
  );
  assert.equal(pci.vendorId, expectedVendorId);
  assert(Number.isInteger(pci.deviceId) && pci.deviceId > 0);
  assert(Number.isInteger(pci.subsystemVendorId));
  assert(Number.isInteger(pci.subsystemDeviceId));
}

function requireLinuxInfo(info, expectedDriver) {
  const linux = info.linux;
  assert(linux !== null && typeof linux === "object" && !Array.isArray(linux));
  assert.equal(linux.driver, expectedDriver);
  assert(Array.isArray(linux.drmNodes));
  assert.equal(new Set(linux.drmNodes).size, linux.drmNodes.length);
  assert(
    linux.drmNodes.some((node) => /^card\d+$/u.test(node)),
    `${expectedDriver} DRM card node missing`,
  );
  assert(
    linux.drmNodes.some((node) => /^renderD\d+$/u.test(node)),
    `${expectedDriver} DRM render node missing`,
  );
  return linux;
}

function assertMetric(metric, label, options = {}) {
  if (metric === undefined) return;
  assert.equal(typeof metric.available, "boolean", `${label}.available`);
  if (!metric.available) {
    assert(
      !("value" in metric),
      `${label} unavailable must not contain a value`,
    );
    assert.equal(typeof metric.reason, "string", `${label}.reason`);
    return;
  }
  assert(Number.isFinite(metric.value), `${label} must be finite`);
  assert.equal(typeof metric.source, "string", `${label}.source`);
  if (options.source !== undefined) {
    assert.equal(metric.source, options.source, `${label}.source`);
  }
  if (options.minimum !== undefined) {
    assert(metric.value >= options.minimum, `${label} below plausible minimum`);
  }
  if (options.maximum !== undefined) {
    assert(metric.value <= options.maximum, `${label} above plausible maximum`);
  }
  if (options.definition === true) {
    assert.equal(typeof metric.definition, "string", `${label}.definition`);
    assert(
      metric.definition.length > 0,
      `${label}.definition must not be empty`,
    );
  }
}

function assertPercentage(metric, label, source) {
  assertMetric(metric, label, { minimum: 0, maximum: 100, source });
}

function metricObservation(metric) {
  if (metric === undefined) return { present: false };
  if (!metric.available) {
    return {
      present: true,
      available: false,
      reason: metric.reason,
      ...(metric.source === undefined ? {} : { source: metric.source }),
    };
  }
  return {
    present: true,
    available: true,
    value: metric.value,
    source: metric.source,
    quality: metric.quality,
    ...(metric.definition === undefined
      ? {}
      : { definition: metric.definition }),
  };
}

function allMetrics(snapshot) {
  return [
    snapshot.utilization.overall,
    snapshot.utilization.graphics,
    snapshot.utilization.compute,
    snapshot.utilization.copy,
    snapshot.utilization.memoryController,
    snapshot.utilization.encoder,
    snapshot.utilization.decoder,
    snapshot.memory.dedicatedUsedBytes,
    snapshot.memory.sharedUsedBytes,
    snapshot.memory.unifiedUsedBytes,
    snapshot.memory.budgetBytes,
    snapshot.memory.bandwidthUtilizationPercent,
    snapshot.temperatures.coreCelsius,
    snapshot.temperatures.edgeCelsius,
    snapshot.temperatures.hotspotCelsius,
    snapshot.temperatures.memoryCelsius,
    snapshot.power.drawWatts,
    snapshot.power.limitWatts,
    snapshot.power.energyJoules,
    snapshot.clocks.graphicsMHz,
    snapshot.clocks.computeMHz,
    snapshot.clocks.memoryMHz,
    snapshot.clocks.videoMHz,
    snapshot.fan.percent,
    snapshot.fan.rpm,
  ].filter((metric) => metric !== undefined);
}

async function testWorkerIsolation(expectedIds) {
  const source = `
    import { parentPort } from "node:worker_threads";
    import { GpuMonitor } from ${JSON.stringify(packageEntry)};
    const monitor = await GpuMonitor.open();
    try {
      const gpus = await monitor.gpus();
      parentPort.postMessage(gpus.map((gpu) => gpu.id).sort());
    } finally {
      await monitor.close();
    }
  `;
  const ids = await withDeadline(
    new Promise((resolvePromise, reject) => {
      const worker = new Worker(source, { eval: true, type: "module" });
      worker.once("message", resolvePromise);
      worker.once("error", reject);
      worker.once("exit", (code) => {
        if (code !== 0)
          reject(new Error(`worker exited with code ${String(code)}`));
      });
    }),
    10_000,
    "worker_threads discovery",
  );
  assert.deepEqual(ids, [...expectedIds].sort());
}

async function environmentReport() {
  let distribution = "Linux";
  try {
    const contents = await readFile("/etc/os-release", "utf8");
    const match = contents.match(/^PRETTY_NAME=(?:"([^"]+)"|([^\n]+))$/mu);
    distribution = match?.[1] ?? match?.[2] ?? distribution;
  } catch {
    // Distribution metadata is optional and unrelated to telemetry behavior.
  }
  const runtimeReport = process.report?.getReport?.();
  return {
    distribution,
    kernel: release(),
    architecture: arch(),
    libc: runtimeReport?.header?.glibcVersionRuntime
      ? `glibc ${runtimeReport.header.glibcVersionRuntime}`
      : "unknown",
    node: process.version,
  };
}

async function testMonitor() {
  const monitor = await GpuMonitor.open();
  let expectedIds;
  const report = { environment: await environmentReport() };
  try {
    const first = await monitor.gpus();
    const second = await monitor.gpus();
    expectedIds = first.map((gpu) => gpu.id).sort();
    assert.deepEqual(second.map((gpu) => gpu.id).sort(), expectedIds);
    assert.equal(new Set(expectedIds).size, first.length, "duplicate GPU IDs");

    const pciAddresses = first
      .map((gpu) => gpu.identity.pci?.address)
      .filter((address) => address !== undefined);
    assert.equal(
      new Set(pciAddresses).size,
      pciAddresses.length,
      "duplicate PCI GPUs",
    );

    const nvidiaGpus = first.filter((gpu) => gpu.vendor === "nvidia");
    const intelGpus = first.filter((gpu) => gpu.vendor === "intel");
    assert(nvidiaGpus.length >= 1);
    assert(intelGpus.length >= 1);
    const nvidia = nvidiaGpus[0];
    const intel = intelGpus[0];
    assertPciIdentity(nvidia, 0x10de);
    assertPciIdentity(intel, 0x8086);
    assert(nvidia.identity.name.trim().length > 0);
    assert(intel.identity.name.trim().length > 0);
    assert.equal(nvidia.identity.kind, "unknown");
    assert.equal(intel.identity.kind, "unknown");
    assert.equal(typeof nvidia.identity.uuid, "string");
    assert(nvidia.identity.uuid.length > 0);

    const nvidiaInfo = await nvidia.nvidiaInfo();
    const intelInfo = await intel.intelInfo();
    const nvidiaLinux = requireLinuxInfo(nvidiaInfo, "nvidia");
    const intelLinux = intelInfo.linux;
    assert(
      intelLinux !== null &&
        typeof intelLinux === "object" &&
        !Array.isArray(intelLinux),
    );
    assert(["i915", "xe"].includes(intelLinux.driver));
    requireLinuxInfo(intelInfo, intelLinux.driver);
    if (nvidiaInfo.cudaComputeCapability !== undefined) {
      assert(Number.isInteger(nvidiaInfo.cudaComputeCapability.major));
      assert(Number.isInteger(nvidiaInfo.cudaComputeCapability.minor));
    }
    if (nvidiaInfo.smCount !== undefined) {
      assert(Number.isInteger(nvidiaInfo.smCount) && nvidiaInfo.smCount > 0);
    }
    assertMetric(nvidiaInfo.pState, "nvidia.info.pState", {
      minimum: 0,
      maximum: 15,
      source: "nvml",
    });
    assertMetric(nvidiaInfo.bar1UsedBytes, "nvidia.info.bar1UsedBytes", {
      minimum: 0,
      maximum: Number.MAX_SAFE_INTEGER,
      source: "nvml",
    });
    if (nvidiaInfo.mig !== undefined) {
      assert.equal(typeof nvidiaInfo.mig.supported, "boolean");
    }
    if (nvidiaInfo.ecc !== undefined) {
      assert.equal(typeof nvidiaInfo.ecc.supported, "boolean");
    }

    const refreshed = await monitor.refresh();
    assert.deepEqual(refreshed.map((gpu) => gpu.id).sort(), expectedIds);

    const concurrentSnapshots = await Promise.all(
      refreshed.map((gpu) => gpu.sample({ includeProcesses: false })),
    );
    assert.equal(concurrentSnapshots.length, refreshed.length);

    const nvidiaWithoutProcesses = await nvidia.sample({
      includeProcesses: false,
    });
    assert.equal(nvidiaWithoutProcesses.processes, undefined);
    const nvidiaSnapshot = await nvidia.sample({ includeProcesses: true });
    const intelSnapshot = await intel.sample({ includeProcesses: true });
    assert.equal(intelSnapshot.processes, undefined);
    if (nvidia.capabilities.processes) {
      assert(Array.isArray(nvidiaSnapshot.processes));
      assert(nvidiaSnapshot.processes.length <= 16_384);
      for (const process of nvidiaSnapshot.processes) {
        assert(Number.isInteger(process.pid) && process.pid >= 0);
        assertMetric(process.memoryUsedBytes, "nvidia.process.memory", {
          minimum: 0,
          maximum: Number.MAX_SAFE_INTEGER,
          source: "nvml",
        });
      }
    } else {
      assert.equal(nvidiaSnapshot.processes, undefined);
    }

    assertPercentage(
      nvidiaSnapshot.utilization.overall,
      "nvidia.overall",
      "nvml",
    );
    assert(
      nvidiaSnapshot.utilization.overall.available,
      "NVML overall unavailable",
    );
    assert.equal(
      typeof nvidiaSnapshot.utilization.overall.definition,
      "string",
    );
    for (const [name, metric] of Object.entries({
      memoryController: nvidiaSnapshot.utilization.memoryController,
      encoder: nvidiaSnapshot.utilization.encoder,
      decoder: nvidiaSnapshot.utilization.decoder,
      fanPercent: nvidiaSnapshot.fan.percent,
    })) {
      assertPercentage(metric, `nvidia.${name}`, "nvml");
    }
    assertMetric(nvidiaSnapshot.memory.dedicatedUsedBytes, "nvidia.vramUsed", {
      minimum: 0,
      maximum: Number.MAX_SAFE_INTEGER,
      source: "nvml",
    });
    assert(
      Number.isSafeInteger(nvidiaSnapshot.memory.dedicatedTotalBytes) &&
        nvidiaSnapshot.memory.dedicatedTotalBytes > 0,
    );
    assertMetric(
      nvidiaSnapshot.temperatures.coreCelsius,
      "nvidia.temperature",
      {
        minimum: -100,
        maximum: 250,
        source: "nvml",
      },
    );
    for (const [name, metric] of Object.entries({
      draw: nvidiaSnapshot.power.drawWatts,
      limit: nvidiaSnapshot.power.limitWatts,
    })) {
      assertMetric(metric, `nvidia.power.${name}`, {
        minimum: 0,
        maximum: 10_000,
        source: "nvml",
      });
    }
    assertMetric(nvidiaSnapshot.power.energyJoules, "nvidia.energy", {
      minimum: 0,
      maximum: Number.MAX_SAFE_INTEGER,
      source: "nvml",
    });
    for (const [name, metric] of Object.entries({
      graphics: nvidiaSnapshot.clocks.graphicsMHz,
      compute: nvidiaSnapshot.clocks.computeMHz,
      memory: nvidiaSnapshot.clocks.memoryMHz,
      video: nvidiaSnapshot.clocks.videoMHz,
    })) {
      assertMetric(metric, `nvidia.clock.${name}`, {
        minimum: 0,
        maximum: 100_000,
        source: "nvml",
      });
    }
    assertMetric(nvidiaSnapshot.fan.rpm, "nvidia.fan.rpm", {
      minimum: 0,
      maximum: 1_000_000,
      source: "nvml",
    });

    assert.equal(intel.capabilities.utilization.overall, false);
    assert.equal(intelSnapshot.utilization.overall.available, false);
    assert.equal(intelSnapshot.utilization.overall.reason, "unsupported");
    for (const [name, metric] of Object.entries({
      clock: intelSnapshot.clocks.graphicsMHz,
      temperature: intelSnapshot.temperatures.coreCelsius,
      power: intelSnapshot.power.drawWatts,
    })) {
      assertMetric(metric, `intel.${name}`, {
        minimum: name === "temperature" ? -100 : 0,
        maximum: name === "temperature" ? 250 : 100_000,
        source: "linux-sysfs",
      });
    }

    for (const metric of [
      ...allMetrics(nvidiaSnapshot),
      ...allMetrics(intelSnapshot),
    ]) {
      if (metric.available) {
        assert(Number.isFinite(metric.value));
      } else {
        assert(!("value" in metric));
      }
    }

    const streamControllers = Array.from(
      { length: 4 },
      () => new AbortController(),
    );
    const streams = streamControllers.map((controller) =>
      nvidia.samples({ intervalMs: 60_000, signal: controller.signal }),
    );
    await Promise.all(streams.map((stream) => stream.next()));
    const pending = streams.map((stream) => stream.next());
    const readStarted = performance.now();
    await withDeadline(readFile(fixturePath), 750, "fs.promises.readFile");
    report.fsReadWhileFourStreamsPendingMs = Math.round(
      performance.now() - readStarted,
    );
    for (const controller of streamControllers) controller.abort();
    for (const result of await withDeadline(
      Promise.all(pending),
      1_000,
      "four-stream cancellation",
    )) {
      assert.deepEqual(result, { value: undefined, done: true });
    }

    let earlyBreakCount = 0;
    for await (const _snapshot of nvidia.samples({ intervalMs: 250 })) {
      earlyBreakCount += 1;
      break;
    }
    assert.equal(earlyBreakCount, 1);

    const abortController = new AbortController();
    const aborted = nvidia.samples({
      intervalMs: 60_000,
      signal: abortController.signal,
    });
    await aborted.next();
    const abortedPending = aborted.next();
    abortController.abort();
    assert.deepEqual(
      await withDeadline(abortedPending, 1_000, "AbortSignal cancellation"),
      { value: undefined, done: true },
    );

    const diagnostics = await monitor.diagnostics();
    assert.equal(provider(diagnostics, "linux-sysfs")?.loaded, true);
    assert(provider(diagnostics, "linux-sysfs").devicesMatched >= 2);
    assert.equal(provider(diagnostics, "nvml")?.loaded, true);
    assert.equal(
      provider(diagnostics, "nvml")?.devicesMatched,
      nvidiaGpus.length,
    );
    for (const selection of diagnostics.metricSelections ?? []) {
      assert(selection.candidates.length > 0);
      assert.equal(
        selection.candidates.filter((candidate) => candidate.selected).length,
        1,
      );
    }
    const nvidiaOverall = diagnostics.metricSelections?.find(
      (selection) =>
        selection.deviceId === nvidia.id &&
        selection.metric === "utilization.overall",
    );
    assert(nvidiaOverall);
    assert(
      nvidiaOverall.candidates.some(
        (candidate) => candidate.source === "nvml" && candidate.selected,
      ),
    );

    report.providers = diagnostics.providers.map((entry) => ({
      id: entry.id,
      loaded: entry.loaded,
      version: entry.version,
      devicesMatched: entry.devicesMatched,
      reason: entry.reason,
    }));
    report.warningCount = diagnostics.warnings.length;
    report.gpus = [
      {
        vendor: "nvidia",
        name: nvidia.identity.name,
        kind: nvidia.identity.kind,
        driverVersion: nvidia.identity.driverVersion,
        pciAddress: nvidia.identity.pci.address,
        drmNodes: nvidiaLinux.drmNodes,
        metrics: {
          overall: metricObservation(nvidiaSnapshot.utilization.overall),
          memoryController: metricObservation(
            nvidiaSnapshot.utilization.memoryController,
          ),
          dedicatedUsedBytes: metricObservation(
            nvidiaSnapshot.memory.dedicatedUsedBytes,
          ),
          temperature: metricObservation(
            nvidiaSnapshot.temperatures.coreCelsius,
          ),
          power: metricObservation(nvidiaSnapshot.power.drawWatts),
          powerLimit: metricObservation(nvidiaSnapshot.power.limitWatts),
          energy: metricObservation(nvidiaSnapshot.power.energyJoules),
          graphicsClock: metricObservation(nvidiaSnapshot.clocks.graphicsMHz),
          computeClock: metricObservation(nvidiaSnapshot.clocks.computeMHz),
          memoryClock: metricObservation(nvidiaSnapshot.clocks.memoryMHz),
          videoClock: metricObservation(nvidiaSnapshot.clocks.videoMHz),
          fanPercent: metricObservation(nvidiaSnapshot.fan.percent),
          fanRpm: metricObservation(nvidiaSnapshot.fan.rpm),
          encoder: metricObservation(nvidiaSnapshot.utilization.encoder),
          decoder: metricObservation(nvidiaSnapshot.utilization.decoder),
          processes:
            nvidiaSnapshot.processes === undefined
              ? { present: false }
              : { present: true, count: nvidiaSnapshot.processes.length },
        },
      },
      {
        vendor: "intel",
        name: intel.identity.name,
        kind: intel.identity.kind,
        driver: intelLinux.driver,
        pciAddress: intel.identity.pci.address,
        drmNodes: intelLinux.drmNodes,
        metrics: {
          overall: metricObservation(intelSnapshot.utilization.overall),
          graphicsClock: metricObservation(intelSnapshot.clocks.graphicsMHz),
          temperature: metricObservation(
            intelSnapshot.temperatures.coreCelsius,
          ),
          power: metricObservation(intelSnapshot.power.drawWatts),
          processes: { present: false },
        },
      },
    ];
  } finally {
    const closeStarted = performance.now();
    await withDeadline(monitor.close(), 3_000, "monitor.close()");
    report.closeMs = Math.round(performance.now() - closeStarted);
  }

  const closeMonitor = await GpuMonitor.open();
  const closeNvidia = (await closeMonitor.gpus()).find(
    (gpu) => gpu.vendor === "nvidia",
  );
  assert(closeNvidia);
  const closeStreams = Array.from({ length: 4 }, () =>
    closeNvidia.samples({ intervalMs: 60_000 }),
  );
  await Promise.all(closeStreams.map((stream) => stream.next()));
  const closePending = closeStreams.map((stream) => stream.next());
  const firstClose = closeMonitor.close();
  assert.equal(firstClose, closeMonitor.close(), "close() must be idempotent");
  await withDeadline(firstClose, 3_000, "close with pending next()");
  for (const result of await withDeadline(
    Promise.all(closePending),
    1_000,
    "pending next() after close",
  )) {
    assert.deepEqual(result, { value: undefined, done: true });
  }
  await assert.rejects(closeMonitor.gpus(), /closed/iu);

  const reopened = await GpuMonitor.open();
  try {
    assert.deepEqual(
      (await reopened.gpus()).map((gpu) => gpu.id).sort(),
      expectedIds,
    );
  } finally {
    await reopened.close();
  }
  await testWorkerIsolation(expectedIds);
  return report;
}

const skipReason = await checkPrerequisites();
if (skipReason !== undefined) {
  console.log(JSON.stringify({ skipped: true, reason: skipReason }));
} else {
  console.log(JSON.stringify(await testMonitor(), null, 2));
}
