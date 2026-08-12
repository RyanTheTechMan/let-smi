import { strict as assert } from "node:assert";
import { readFile } from "node:fs/promises";
import { Worker } from "node:worker_threads";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageEntry = pathToFileURL(
  resolve(repositoryRoot, "packages/gpu/dist/index.js"),
).href;
const fixturePath = fileURLToPath(import.meta.url);
const { GpuMonitor } = await import(packageEntry);

const sleep = (milliseconds) =>
  new Promise((resolvePromise) => setTimeout(resolvePromise, milliseconds));

async function checkPrerequisites() {
  if (process.platform !== "win32" || process.arch !== "x64") {
    return "requires Windows x64";
  }
  const monitor = await GpuMonitor.open();
  try {
    const gpus = await monitor.gpus();
    const diagnostics = await monitor.diagnostics();
    const providerLoaded = (id) =>
      diagnostics.providers.some(
        (provider) => provider.id === id && provider.loaded,
      );
    if (
      gpus.length !== 2 ||
      !gpus.some((gpu) => gpu.vendor === "intel") ||
      !gpus.some((gpu) => gpu.vendor === "nvidia")
    ) {
      return "requires exactly one Intel and one NVIDIA physical adapter";
    }
    if (!providerLoaded("windows-pdh") || !providerLoaded("nvml")) {
      return "requires functional Windows PDH and NVML providers";
    }
    return undefined;
  } finally {
    await monitor.close();
  }
}

async function withDeadline(promise, milliseconds, label) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} exceeded ${milliseconds} ms`)),
          milliseconds,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function assertMetric(metric, label) {
  assert.equal(typeof metric.available, "boolean", `${label}.available`);
  if (metric.available) {
    assert(Number.isFinite(metric.value), `${label} must be finite`);
    assert.equal(typeof metric.source, "string", `${label}.source`);
  } else {
    assert.notEqual(metric.value, 0, `${label} unavailable must not be zero`);
    assert.equal(typeof metric.reason, "string", `${label}.reason`);
  }
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
    ...(metric.intervalMs === undefined
      ? {}
      : { intervalMs: metric.intervalMs }),
  };
}

async function testWorkerIsolation() {
  const source = `
    import { parentPort } from "node:worker_threads";
    import { GpuMonitor } from ${JSON.stringify(packageEntry)};
    const monitor = await GpuMonitor.open();
    try {
      const gpus = await monitor.gpus();
      parentPort.postMessage({ count: gpus.length, ids: gpus.map((gpu) => gpu.id) });
    } finally {
      await monitor.close();
    }
  `;
  const result = await withDeadline(
    new Promise((resolvePromise, reject) => {
      const worker = new Worker(source, { eval: true, type: "module" });
      worker.once("message", resolvePromise);
      worker.once("error", reject);
      worker.once("exit", (code) => {
        if (code !== 0) reject(new Error(`worker exited with code ${code}`));
      });
    }),
    10_000,
    "worker isolation",
  );
  assert.equal(result.count, 2);
  assert.equal(new Set(result.ids).size, 2);
}

async function testMonitor() {
  let expectedIds;
  let firstPdhIntervalMs;
  const firstSampleMonitor = await GpuMonitor.open();
  const firstSampleIntel = (await firstSampleMonitor.gpus()).find(
    (gpu) => gpu.vendor === "intel",
  );
  assert(firstSampleIntel);
  const firstSampleStream = firstSampleIntel.samples({ intervalMs: 250 });
  const firstPdhSnapshot = await firstSampleStream.next();
  assert.equal(firstPdhSnapshot.done, false);
  assert.equal(firstPdhSnapshot.value.utilization.overall.available, false);
  assert.equal(
    firstPdhSnapshot.value.utilization.overall.reason,
    "first-sample",
  );
  assert.equal(firstPdhSnapshot.value.processes, undefined);
  for (const [name, metric] of Object.entries({
    graphics: firstPdhSnapshot.value.utilization.graphics,
    compute: firstPdhSnapshot.value.utilization.compute,
    copy: firstPdhSnapshot.value.utilization.copy,
    encoder: firstPdhSnapshot.value.utilization.encoder,
    decoder: firstPdhSnapshot.value.utilization.decoder,
  })) {
    assert(metric, `Intel first sample is missing ${name}`);
    assert.equal(metric.available, false, `${name} must need a baseline`);
    assert.equal(metric.reason, "first-sample", `${name} first-sample reason`);
  }
  const secondPdhSnapshot = await firstSampleStream.next();
  assert.equal(secondPdhSnapshot.done, false);
  assertMetric(
    secondPdhSnapshot.value.utilization.overall,
    "intel.secondPdhOverall",
  );
  firstPdhIntervalMs = secondPdhSnapshot.value.utilization.overall.intervalMs;
  assert(
    Number.isFinite(firstPdhIntervalMs) && firstPdhIntervalMs > 0,
    "the second PDH sample must expose its measured interval",
  );
  await firstSampleStream.return();
  await firstSampleMonitor.close();

  const monitor = await GpuMonitor.open();
  const report = {};
  try {
    const first = await monitor.gpus();
    const second = await monitor.gpus();
    assert.equal(
      first.length,
      2,
      "hybrid system must expose two physical GPUs",
    );
    expectedIds = first.map((gpu) => gpu.id);
    assert.deepEqual(
      second.map((gpu) => gpu.id),
      first.map((gpu) => gpu.id),
      "repeated enumeration must keep stable IDs",
    );
    const intel = first.find((gpu) => gpu.vendor === "intel");
    const nvidia = first.find((gpu) => gpu.vendor === "nvidia");
    assert(intel, "Intel GPU not discovered");
    assert(nvidia, "NVIDIA GPU not discovered");
    assert.match(intel.identity.name, /intel/iu);
    assert.match(nvidia.identity.name, /nvidia/iu);
    assert.equal(intel.identity.kind, "integrated");
    assert.equal(nvidia.identity.kind, "discrete");
    assert(intel.identity.pci?.address, "Intel PCI identity missing");
    assert(nvidia.identity.pci?.address, "NVIDIA PCI identity missing");
    assert(intel.identity.windows?.luid, "Intel LUID missing");
    assert(nvidia.identity.windows?.luid, "NVIDIA LUID missing");
    assert.match(intel.identity.windows.luid, /^[0-9a-f]{8}:[0-9a-f]{8}$/u);
    assert.match(nvidia.identity.windows.luid, /^[0-9a-f]{8}:[0-9a-f]{8}$/u);
    assert.notEqual(intel.identity.windows.luid, nvidia.identity.windows.luid);
    assert.equal(intel.identity.pci.vendorId, 0x8086);
    assert.equal(nvidia.identity.pci.vendorId, 0x10de);
    assert(intel.identity.pci.deviceId > 0, "Intel device ID missing");
    assert(nvidia.identity.pci.deviceId > 0, "NVIDIA device ID missing");
    assert(
      Number.isInteger(intel.identity.pci.subsystemVendorId),
      "Intel subsystem vendor ID missing",
    );
    assert(
      Number.isInteger(intel.identity.pci.subsystemDeviceId),
      "Intel subsystem device ID missing",
    );
    assert(
      Number.isInteger(nvidia.identity.pci.subsystemVendorId),
      "NVIDIA subsystem vendor ID missing",
    );
    assert(
      Number.isInteger(nvidia.identity.pci.subsystemDeviceId),
      "NVIDIA subsystem device ID missing",
    );
    assert.equal(typeof nvidia.identity.uuid, "string");

    const refreshed = await monitor.refresh();
    assert.deepEqual(
      refreshed.map((gpu) => gpu.id),
      first.map((gpu) => gpu.id),
      "refresh must preserve IDs",
    );

    const snapshots = await Promise.all(
      refreshed.map((gpu) => gpu.sample({ includeProcesses: true })),
    );
    for (const [index, snapshot] of snapshots.entries()) {
      assertMetric(snapshot.utilization.overall, `gpu[${index}].overall`);
    }
    const intelSnapshot =
      snapshots[refreshed.findIndex((gpu) => gpu.id === intel.id)];
    const nvidiaSnapshot =
      snapshots[refreshed.findIndex((gpu) => gpu.id === nvidia.id)];
    assert.notEqual(intelSnapshot.memory.topology, "unknown");
    assert.notEqual(nvidiaSnapshot.memory.topology, "unknown");
    assert(
      (intelSnapshot.memory.sharedTotalBytes ?? 0) > 0,
      "Intel shared memory topology is missing",
    );
    assert(
      (nvidiaSnapshot.memory.dedicatedTotalBytes ?? 0) > 0,
      "NVIDIA dedicated memory topology is missing",
    );
    if (intelSnapshot.utilization.overall.available) {
      assert.equal(intelSnapshot.utilization.overall.source, "windows-pdh");
    }
    if (nvidiaSnapshot.utilization.overall.available) {
      assert.equal(nvidiaSnapshot.utilization.overall.source, "nvml");
    }
    for (const [name, metric] of Object.entries({
      memoryUsed: nvidiaSnapshot.memory.dedicatedUsedBytes,
      temperature: nvidiaSnapshot.temperatures.coreCelsius,
      power: nvidiaSnapshot.power.drawWatts,
      graphicsClock: nvidiaSnapshot.clocks.graphicsMHz,
      memoryClock: nvidiaSnapshot.clocks.memoryMHz,
      fan: nvidiaSnapshot.fan.percent,
      encoder: nvidiaSnapshot.utilization.encoder,
      decoder: nvidiaSnapshot.utilization.decoder,
    })) {
      if (metric !== undefined) {
        assertMetric(metric, `nvidia.${name}`);
      }
    }
    if (nvidiaSnapshot.processes !== undefined) {
      assert(Array.isArray(nvidiaSnapshot.processes));
    }
    assert.equal(
      intelSnapshot.processes,
      undefined,
      "unsupported Intel process telemetry must be omitted",
    );
    const nvidiaInfo = await nvidia.nvidiaInfo();
    assert.equal(typeof nvidiaInfo, "object");
    if (nvidiaInfo.vbiosVersion !== undefined) {
      assert.equal(nvidiaInfo.vbiosVersion, nvidia.identity.firmwareVersion);
    }
    assert.deepEqual(await intel.intelInfo(), {});

    report.observedMetrics = {
      intel: {
        overall: metricObservation(intelSnapshot.utilization.overall),
        graphics: metricObservation(intelSnapshot.utilization.graphics),
        compute: metricObservation(intelSnapshot.utilization.compute),
        copy: metricObservation(intelSnapshot.utilization.copy),
        encoder: metricObservation(intelSnapshot.utilization.encoder),
        decoder: metricObservation(intelSnapshot.utilization.decoder),
        temperature: metricObservation(intelSnapshot.temperatures.coreCelsius),
        power: metricObservation(intelSnapshot.power.drawWatts),
        clocks: metricObservation(intelSnapshot.clocks.graphicsMHz),
        processes: { present: false },
      },
      nvidia: {
        overall: metricObservation(nvidiaSnapshot.utilization.overall),
        graphics: metricObservation(nvidiaSnapshot.utilization.graphics),
        memoryController: metricObservation(
          nvidiaSnapshot.utilization.memoryController,
        ),
        compute: metricObservation(nvidiaSnapshot.utilization.compute),
        copy: metricObservation(nvidiaSnapshot.utilization.copy),
        encoder: metricObservation(nvidiaSnapshot.utilization.encoder),
        decoder: metricObservation(nvidiaSnapshot.utilization.decoder),
        dedicatedTotalBytes: nvidiaSnapshot.memory.dedicatedTotalBytes,
        dedicatedUsedBytes: metricObservation(
          nvidiaSnapshot.memory.dedicatedUsedBytes,
        ),
        temperature: metricObservation(nvidiaSnapshot.temperatures.coreCelsius),
        power: metricObservation(nvidiaSnapshot.power.drawWatts),
        powerLimit: metricObservation(nvidiaSnapshot.power.limitWatts),
        energy: metricObservation(nvidiaSnapshot.power.energyJoules),
        graphicsClock: metricObservation(nvidiaSnapshot.clocks.graphicsMHz),
        computeClock: metricObservation(nvidiaSnapshot.clocks.computeMHz),
        memoryClock: metricObservation(nvidiaSnapshot.clocks.memoryMHz),
        videoClock: metricObservation(nvidiaSnapshot.clocks.videoMHz),
        fanPercent: metricObservation(nvidiaSnapshot.fan.percent),
        fanRpm: metricObservation(nvidiaSnapshot.fan.rpm),
        processes: {
          present: nvidiaSnapshot.processes !== undefined,
          ...(nvidiaSnapshot.processes === undefined
            ? {}
            : { count: nvidiaSnapshot.processes.length }),
        },
      },
    };
    report.nvidiaVendorInfo = {
      architecture: nvidia.identity.architecture,
      driverVersion: nvidia.identity.driverVersion,
      cudaComputeCapability: nvidiaInfo.cudaComputeCapability,
      hasVbiosVersion: typeof nvidiaInfo.vbiosVersion === "string",
      hasPcie: nvidiaInfo.pcieGeneration !== undefined,
      hasPState: nvidiaInfo.pState !== undefined,
    };

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
    // Calling return() on an async generator is queued behind its pending
    // next(). AbortSignals synchronously request native cancellation instead.
    for (const controller of streamControllers) controller.abort();
    await Promise.all(pending);

    const abortController = new AbortController();
    const abortedStream = nvidia.samples({
      intervalMs: 60_000,
      signal: abortController.signal,
    });
    await abortedStream.next();
    const abortedPending = abortedStream.next();
    abortController.abort();
    assert.deepEqual(
      await withDeadline(abortedPending, 1_000, "AbortSignal cancellation"),
      { value: undefined, done: true },
    );

    const sharedFirst = nvidia.samples({ intervalMs: 1_000 });
    const sharedSecond = nvidia.samples({ intervalMs: 1_000 });
    await Promise.all([sharedFirst.next(), sharedSecond.next()]);
    await Promise.all([sharedFirst.return(), sharedSecond.return()]);

    let earlyBreakCount = 0;
    for await (const _snapshot of nvidia.samples({ intervalMs: 250 })) {
      earlyBreakCount += 1;
      break;
    }
    assert.equal(
      earlyBreakCount,
      1,
      "early break must cancel its subscription",
    );

    const diagnostics = await monitor.diagnostics();
    report.providers = diagnostics.providers;
    const provider = (id) =>
      diagnostics.providers.find((candidate) => candidate.id === id);
    assert.equal(provider("windows-dxgi")?.loaded, true);
    assert.equal(provider("windows-dxgi")?.devicesMatched, 2);
    assert.equal(provider("windows-pdh")?.loaded, true);
    assert.equal(provider("windows-pdh")?.devicesMatched, 2);
    assert.equal(provider("nvml")?.loaded, true);
    assert.equal(provider("nvml")?.devicesMatched, 1);
    assert.match(
      provider("nvml")?.message ?? "",
      /NVML loaded securely from the Windows system directory|NVML loaded securely from the NVIDIA NVSMI directory under Program Files/u,
    );
    assert.equal(provider("amd-adlx")?.loaded, false);
    assert.equal(provider("level-zero")?.loaded, false);
    for (const selection of diagnostics.metricSelections ?? []) {
      assert(selection.candidates.length > 0);
      assert.equal(
        selection.candidates.filter((candidate) => candidate.selected).length,
        1,
        `${selection.metric} must have exactly one selected candidate`,
      );
    }
    const nvidiaOverall = diagnostics.metricSelections?.find(
      (selection) =>
        selection.deviceId === nvidia.id &&
        selection.metric === "utilization.overall",
    );
    assert(nvidiaOverall, "NVIDIA overall merge diagnostics are missing");
    const nvmlCandidate = nvidiaOverall.candidates.find(
      (candidate) => candidate.source === "nvml",
    );
    assert(nvmlCandidate, "NVML overall candidate is missing");
    assert(nvmlCandidate.selected, "NVML must win when it supplies the field");
    assert(
      nvidiaOverall.candidates.some(
        (candidate) => candidate.source === "windows-pdh",
      ),
      "PDH must remain visible as the NVIDIA fallback candidate",
    );
    const intelOverall = diagnostics.metricSelections?.find(
      (selection) =>
        selection.deviceId === intel.id &&
        selection.metric === "utilization.overall",
    );
    assert(intelOverall, "Intel overall merge diagnostics are missing");
    assert(
      intelOverall.candidates.some(
        (candidate) => candidate.source === "windows-pdh" && candidate.selected,
      ),
      "PDH must supply Intel overall utilization",
    );
    report.firstPdhIntervalMs = firstPdhIntervalMs;
    report.warningCount = diagnostics.warnings.length;
    report.overallSelections = {
      intel: intelOverall.candidates
        .filter((candidate) => candidate.selected)
        .map((candidate) => candidate.source),
      nvidia: nvidiaOverall.candidates.map((candidate) => ({
        source: candidate.source,
        selected: candidate.selected,
      })),
    };
    report.gpus = refreshed.map((gpu) => ({
      vendor: gpu.vendor,
      name: gpu.identity.name,
      kind: gpu.identity.kind,
      pciAddress: gpu.identity.pci?.address,
      pciVendorId: gpu.identity.pci?.vendorId,
      pciDeviceId: gpu.identity.pci?.deviceId,
      subsystemVendorId: gpu.identity.pci?.subsystemVendorId,
      subsystemDeviceId: gpu.identity.pci?.subsystemDeviceId,
    }));
  } finally {
    const closeStarted = performance.now();
    await withDeadline(monitor.close(), 3_000, "monitor.close()");
    report.closeMs = Math.round(performance.now() - closeStarted);
  }

  const closeMonitor = await GpuMonitor.open();
  const closeGpu = (await closeMonitor.gpus()).find(
    (gpu) => gpu.vendor === "nvidia",
  );
  assert(closeGpu);
  const closeStreams = Array.from({ length: 4 }, () =>
    closeGpu.samples({ intervalMs: 60_000 }),
  );
  await Promise.all(closeStreams.map((stream) => stream.next()));
  const closePending = closeStreams.map((stream) => stream.next());
  await withDeadline(closeMonitor.close(), 3_000, "close with pending next()");
  const closedResults = await withDeadline(
    Promise.all(closePending),
    1_000,
    "pending next calls after close",
  );
  for (const result of closedResults) {
    assert.deepEqual(result, { value: undefined, done: true });
  }

  const reopenedMonitor = await GpuMonitor.open();
  try {
    assert.deepEqual(
      (await reopenedMonitor.gpus()).map((gpu) => gpu.id),
      expectedIds,
      "monitor reopen must preserve stable IDs",
    );
  } finally {
    await reopenedMonitor.close();
  }
  return report;
}

const skipReason = await checkPrerequisites();
if (skipReason !== undefined) {
  console.log(JSON.stringify({ skipped: true, reason: skipReason }));
} else {
  const report = await testMonitor();
  await testWorkerIsolation();
  await sleep(25);
  console.log(JSON.stringify(report, null, 2));
}
