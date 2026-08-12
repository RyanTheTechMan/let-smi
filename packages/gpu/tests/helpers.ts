import type {
  NativeBinding,
  NativeGpuSubscription,
  NativeMonitorHandle,
} from "../src/native-binding.js";
import type { GpuIdentity, GpuSnapshot, Metric } from "../src/types.js";

export function available(
  value: number,
  overrides: Partial<
    Extract<Metric<number>, { readonly available: true }>
  > = {},
): Metric<number> {
  return {
    available: true,
    value,
    source: "mock-direct",
    quality: "direct",
    sampledAt: 1_720_000_000_000,
    ...overrides,
  };
}

export function snapshot(overall: Metric<number> = available(0)): GpuSnapshot {
  return {
    sampledAt: 1_720_000_000_000,
    utilization: { overall },
    memory: { topology: "dedicated", dedicatedTotalBytes: 8 * 1024 ** 3 },
    temperatures: {},
    power: {},
    clocks: {},
    fan: {},
  };
}

export function identity(
  vendor: GpuIdentity["vendor"] = "nvidia",
  id = `${vendor}:0000:01:00.0`,
): GpuIdentity {
  return {
    id,
    vendor,
    name: `${vendor} test GPU`,
    kind: vendor === "apple" || vendor === "intel" ? "integrated" : "discrete",
    pci: {
      address: "0000:01:00.0",
      vendorId:
        vendor === "nvidia"
          ? 0x10de
          : vendor === "amd"
            ? 0x1002
            : vendor === "intel"
              ? 0x8086
              : 0,
      deviceId: 0x1234,
    },
  };
}

export class QueueSubscription implements NativeGpuSubscription {
  readonly #queue: unknown[];
  #pendingResolve: ((value: unknown) => void) | undefined;
  cancelCalls = 0;

  constructor(values: unknown[] = []) {
    this.#queue = [...values];
  }

  next(): Promise<unknown> {
    if (this.#queue.length > 0) return Promise.resolve(this.#queue.shift());
    return new Promise((resolve) => {
      this.#pendingResolve = resolve;
    });
  }

  cancel(): void {
    this.cancelCalls += 1;
    this.#pendingResolve?.(null);
    this.#pendingResolve = undefined;
  }
}

export class FakeMonitor implements NativeMonitorHandle {
  gpuDescriptors: unknown[] = [
    {
      identity: identity(),
      capabilities: {
        metrics: ["utilization.overall"],
        vendorExtensions: ["nvidia.nvml"],
      },
    },
  ];
  sampleValue: unknown = snapshot();
  diagnosticsValue: unknown = {
    platform: "linux",
    arch: "x64",
    providers: [{ id: "mock", loaded: true, devicesMatched: 1 }],
    warnings: [],
  };
  vendorInfoValue: unknown = { vendor: "nvidia", info: { smCount: 128 } };
  subscription: QueueSubscription = new QueueSubscription([snapshot(), null]);
  listCalls = 0;
  sampleCalls: Array<{
    id: string;
    options: {
      readonly windowMs?: number;
      readonly includeProcesses?: boolean;
    };
  }> = [];
  subscribeCalls: Array<{
    id: string;
    options: {
      readonly intervalMs?: number;
      readonly includeProcesses?: boolean;
    };
  }> = [];
  vendorInfoCalls: string[] = [];
  refreshCalls = 0;
  closeCalls = 0;

  listGpus(): unknown {
    this.listCalls += 1;
    return this.gpuDescriptors;
  }

  sampleGpu(
    id: string,
    options: {
      readonly windowMs?: number;
      readonly includeProcesses?: boolean;
    },
  ): unknown {
    this.sampleCalls.push({ id, options });
    return this.sampleValue;
  }

  subscribeGpu(
    id: string,
    options: {
      readonly intervalMs?: number;
      readonly includeProcesses?: boolean;
    },
  ): unknown {
    this.subscribeCalls.push({ id, options });
    return this.subscription;
  }

  vendorInfo(id: string): unknown {
    this.vendorInfoCalls.push(id);
    return this.vendorInfoValue;
  }

  diagnostics(): unknown {
    return this.diagnosticsValue;
  }

  refresh(): void {
    this.refreshCalls += 1;
  }

  close(): void {
    this.closeCalls += 1;
  }
}

export class FakeBinding implements NativeBinding {
  readonly monitor: FakeMonitor;
  openCalls: unknown[] = [];

  constructor(monitor = new FakeMonitor()) {
    this.monitor = monitor;
  }

  openMonitor(options?: {
    readonly requiredProviders?: readonly string[];
    readonly enableApplePrivateTelemetry?: boolean;
    readonly includeSoftwareAdapters?: boolean;
  }): unknown {
    this.openCalls.push(options);
    return this.monitor;
  }
}
