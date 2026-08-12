import { afterEach, describe, expect, it } from "vitest";

import {
  AmdGpu,
  AppleGpu,
  GpuInvalidArgumentError,
  type Gpu,
  type MonitorOpenOptions,
  GpuMonitor,
  GpuMonitorClosedError,
  GpuNativeDataError,
  IntelGpu,
  NvidiaGpu,
  UnknownGpu,
} from "../src/index.js";
import { setNativeBindingForTesting } from "../src/native-binding.js";
import {
  available,
  FakeBinding,
  FakeMonitor,
  identity,
  QueueSubscription,
  snapshot,
} from "./helpers.js";

afterEach(() => {
  setNativeBindingForTesting(undefined);
});

async function openFake(monitor = new FakeMonitor()): Promise<{
  monitor: FakeMonitor;
  api: GpuMonitor;
  binding: FakeBinding;
}> {
  const binding = new FakeBinding(monitor);
  setNativeBindingForTesting(binding);
  return { monitor, api: await GpuMonitor.open(), binding };
}

async function firstGpu(api: GpuMonitor): Promise<Gpu> {
  const gpu = (await api.gpus())[0];
  if (gpu === undefined) throw new Error("test setup did not provide a GPU");
  return gpu;
}

describe("GpuMonitor discovery", () => {
  it("constructs a discriminated vendor subclass for every identity", async () => {
    const native = new FakeMonitor();
    native.gpuDescriptors = ["nvidia", "amd", "intel", "apple", "unknown"].map(
      (vendor, index) => ({
        identity: identity(
          vendor as "nvidia" | "amd" | "intel" | "apple" | "unknown",
          `gpu-${String(index)}`,
        ),
        capabilities: {
          metrics:
            index === 0 ? ["utilization.overall", "power.drawWatts"] : [],
        },
      }),
    );
    const { api } = await openFake(native);

    const gpus = await api.gpus();
    expect(gpus).toHaveLength(5);
    expect(gpus[0]).toBeInstanceOf(NvidiaGpu);
    expect(gpus[1]).toBeInstanceOf(AmdGpu);
    expect(gpus[2]).toBeInstanceOf(IntelGpu);
    expect(gpus[3]).toBeInstanceOf(AppleGpu);
    expect(gpus[4]).toBeInstanceOf(UnknownGpu);
    expect(gpus[0]?.supports("power.drawWatts")).toBe(true);
    expect(gpus[0]?.capabilities.power).toBe(true);
    expect(Object.isFrozen(gpus[0]?.identity)).toBe(true);

    const secondRead = await api.gpus();
    expect(native.listCalls).toBe(1);
    expect(secondRead).not.toBe(gpus);
    expect(secondRead[0]).toBe(gpus[0]);
    await api.close();
  });

  it("normalizes required providers at open", async () => {
    const binding = new FakeBinding();
    setNativeBindingForTesting(binding);
    const api = await GpuMonitor.open({
      requiredProviders: ["nvml", "nvml"],
      enableApplePrivateTelemetry: true,
      includeSoftwareAdapters: false,
    });
    expect(binding.openCalls).toEqual([
      {
        requiredProviders: ["nvml"],
        enableApplePrivateTelemetry: true,
        includeSoftwareAdapters: false,
      },
    ]);
    await expect(
      GpuMonitor.open(null as unknown as MonitorOpenOptions),
    ).rejects.toBeInstanceOf(GpuInvalidArgumentError);
    await api.close();
  });

  it("rejects malformed native identities and duplicate stable ids", async () => {
    const malformed = new FakeMonitor();
    malformed.gpuDescriptors = [
      { identity: { ...identity(), vendor: "bogus" }, capabilities: {} },
    ];
    const first = await openFake(malformed);
    await expect(first.api.gpus()).rejects.toBeInstanceOf(GpuNativeDataError);
    await first.api.close();

    const duplicate = new FakeMonitor();
    duplicate.gpuDescriptors = [
      identity("amd", "same"),
      identity("amd", "same"),
    ];
    const second = await openFake(duplicate);
    await expect(second.api.gpus()).rejects.toThrow(
      /duplicate stable device id/,
    );
    await second.api.close();
  });

  it("refreshes topology and invalidates the identity cache", async () => {
    const { api, monitor } = await openFake();
    expect(await api.gpus()).toHaveLength(1);
    monitor.gpuDescriptors = [
      { identity: identity("amd", "amd-new"), capabilities: { metrics: [] } },
    ];
    const refreshed = await api.refresh();
    expect(monitor.refreshCalls).toBe(1);
    expect(monitor.listCalls).toBe(2);
    expect(refreshed[0]).toBeInstanceOf(AmdGpu);
    await api.close();
  });
});

describe("sampling and extensions", () => {
  it("preserves an available zero and an unavailable metric distinctly", async () => {
    const { api, monitor } = await openFake();
    const gpu = await firstGpu(api);

    monitor.sampleValue = snapshot(available(0));
    const zero = await gpu.sample({ windowMs: 250, includeProcesses: true });
    expect(zero.utilization.overall).toMatchObject({
      available: true,
      value: 0,
    });
    expect(monitor.sampleCalls[0]?.options).toEqual({
      windowMs: 250,
      includeProcesses: true,
    });

    monitor.sampleValue = snapshot({
      available: false,
      reason: "first-sample",
      source: "windows-pdh",
    });
    const unavailable = await gpu.sample();
    expect(unavailable.utilization.overall).toEqual({
      available: false,
      reason: "first-sample",
      source: "windows-pdh",
    });

    monitor.sampleValue = {
      ...snapshot(),
      memory: { topology: "unknown" },
    };
    await expect(gpu.sample()).resolves.toMatchObject({
      memory: { topology: "unknown" },
    });
    await api.close();
  });

  it("rejects invalid native metrics and invalid public options", async () => {
    const { api, monitor } = await openFake();
    const gpu = await firstGpu(api);
    monitor.sampleValue = snapshot(available(100.01));
    await expect(gpu.sample()).rejects.toBeInstanceOf(GpuNativeDataError);
    expect(() => gpu.sample({ windowMs: 0 })).toThrow(GpuInvalidArgumentError);
    expect(() =>
      gpu.sample({ includeProcesses: "yes" as unknown as boolean }),
    ).toThrow(GpuInvalidArgumentError);
    await api.close();
  });

  it("returns vendor information only from the matching subclass", async () => {
    const { api, monitor } = await openFake();
    const gpu = await firstGpu(api);
    expect(gpu).toBeInstanceOf(NvidiaGpu);
    if (!(gpu instanceof NvidiaGpu)) throw new Error("test setup failed");
    await expect(gpu.nvidiaInfo()).resolves.toEqual({ smCount: 128 });
    expect(monitor.vendorInfoCalls).toEqual([gpu.id]);

    monitor.vendorInfoValue = { vendor: "NVIDIA Corporation", smCount: 128 };
    await expect(gpu.nvidiaInfo()).resolves.toEqual({
      vendor: "NVIDIA Corporation",
      smCount: 128,
    });
    await api.close();
  });

  it("validates diagnostics", async () => {
    const { api, monitor } = await openFake();
    monitor.diagnosticsValue = {
      platform: "linux",
      arch: "x64",
      providers: [{ id: "mock", loaded: true, devicesMatched: 1 }],
      warnings: [],
      metricSelections: [
        {
          deviceId: "nvidia:0000:01:00.0",
          metrics: [
            {
              metric: "utilization.overall",
              candidates: [
                {
                  source: "nvml",
                  score: 1010.5,
                  selected: true,
                  quality: "direct",
                  sampledAt: 1_720_000_000_000,
                },
              ],
            },
          ],
        },
      ],
    };
    await expect(api.diagnostics()).resolves.toMatchObject({
      platform: "linux",
      providers: [{ id: "mock", loaded: true, devicesMatched: 1 }],
      metricSelections: [
        {
          metric: "utilization.overall",
          candidates: [{ source: "nvml", selected: true }],
        },
      ],
    });
    monitor.diagnosticsValue = {
      platform: "linux",
      arch: "x64",
      providers: {},
    };
    await expect(api.diagnostics()).rejects.toBeInstanceOf(GpuNativeDataError);
    await api.close();
  });
});

describe("continuous sampling", () => {
  it("iterates snapshots and cancels once at natural completion", async () => {
    const native = new FakeMonitor();
    native.subscription = new QueueSubscription([
      snapshot(available(10)),
      snapshot(available(20)),
      null,
    ]);
    const { api } = await openFake(native);
    const gpu = await firstGpu(api);
    const values: number[] = [];
    for await (const value of gpu.samples({
      intervalMs: 50,
      includeProcesses: true,
    })) {
      if (value.utilization.overall.available) {
        values.push(value.utilization.overall.value);
      }
    }
    expect(values).toEqual([10, 20]);
    expect(native.subscribeCalls[0]?.options).toEqual({
      intervalMs: 50,
      includeProcesses: true,
    });
    expect(native.subscription.cancelCalls).toBe(1);
    await api.close();
  });

  it("cancels exactly once when the consumer stops early", async () => {
    const native = new FakeMonitor();
    native.subscription = new QueueSubscription([snapshot(available(42))]);
    const { api } = await openFake(native);
    const gpu = await firstGpu(api);
    for await (const value of gpu.samples()) {
      expect(value.sampledAt).toBeGreaterThan(0);
      break;
    }
    expect(native.subscription.cancelCalls).toBe(1);
    await api.close();
  });

  it("does not subscribe for an already-aborted signal", async () => {
    const { api, monitor } = await openFake();
    const gpu = await firstGpu(api);
    const controller = new AbortController();
    controller.abort();
    const result = await gpu.samples({ signal: controller.signal }).next();
    expect(result.done).toBe(true);
    expect(monitor.subscribeCalls).toHaveLength(0);
    await api.close();
  });

  it("cancels a pending native next when aborted", async () => {
    const native = new FakeMonitor();
    native.subscription = new QueueSubscription();
    const { api } = await openFake(native);
    const gpu = await firstGpu(api);
    const controller = new AbortController();
    const iterator = gpu.samples({ signal: controller.signal });
    const pending = iterator.next();
    await Promise.resolve();
    controller.abort();
    await expect(pending).resolves.toMatchObject({ done: true });
    expect(native.subscription.cancelCalls).toBe(1);
    await api.close();
  });
});

describe("shutdown", () => {
  it("is idempotent, cancels active streams, and rejects later use", async () => {
    const native = new FakeMonitor();
    native.subscription = new QueueSubscription();
    const { api } = await openFake(native);
    const gpu = await firstGpu(api);
    const iterator = gpu.samples();
    const pendingNext = iterator.next();
    await Promise.resolve();

    const firstClose = api.close();
    const secondClose = api.close();
    expect(secondClose).toBe(firstClose);
    await firstClose;
    await expect(pendingNext).resolves.toMatchObject({ done: true });
    expect(native.subscription.cancelCalls).toBe(1);
    expect(native.closeCalls).toBe(1);
    await expect(api.gpus()).rejects.toBeInstanceOf(GpuMonitorClosedError);
    expect(() => gpu.sample()).toThrow(GpuMonitorClosedError);
    await expect(api.diagnostics()).rejects.toBeInstanceOf(
      GpuMonitorClosedError,
    );
  });
});
