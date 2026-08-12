import {
  createGpu,
  type Gpu,
  type GpuClient,
  type GpuSubscription,
} from "./gpu.js";
import {
  assertNativeMonitor,
  assertNativeSubscription,
  getNativeBinding,
  type NativeGpuSubscription,
  type NativeMonitorHandle,
} from "./native-binding.js";
import type {
  GpuDiagnostics,
  GpuSnapshot,
  GpuVendor,
  MonitorOpenOptions,
} from "./types.js";
import { GpuMonitorClosedError, GpuNativeDataError } from "./types.js";
import {
  isRecord,
  normalizeOpenOptions,
  parseGpuCapabilities,
  parseGpuDiagnostics,
  parseGpuIdentity,
  parseGpuSnapshot,
  vendorInfoRecord,
} from "./validation.js";

type MonitorState = "open" | "closing" | "closed";
const MAX_GPU_DESCRIPTORS = 1_024;

class ManagedSubscription implements GpuSubscription {
  readonly #native: NativeGpuSubscription;
  readonly #onFinished: (subscription: ManagedSubscription) => void;
  #cancelled = false;
  #cancelPromise: Promise<void> | undefined;

  constructor(
    native: NativeGpuSubscription,
    onFinished: (subscription: ManagedSubscription) => void,
  ) {
    this.#native = native;
    this.#onFinished = onFinished;
  }

  #isCancelled(): boolean {
    return this.#cancelled;
  }

  async next(): Promise<GpuSnapshot | null> {
    if (this.#isCancelled()) return null;
    try {
      const value = await this.#native.next();
      if (this.#isCancelled()) return null;
      if (value === null) {
        await this.cancel();
        return null;
      }
      return parseGpuSnapshot(value, "subscription.next");
    } catch (error) {
      if (this.#isCancelled()) return null;
      throw error;
    }
  }

  cancel(): Promise<void> {
    if (this.#cancelPromise !== undefined) return this.#cancelPromise;
    this.#cancelled = true;
    this.#cancelPromise = Promise.resolve()
      .then(() => this.#native.cancel())
      .then(() => undefined)
      .finally(() => this.#onFinished(this));
    return this.#cancelPromise;
  }
}

function parseGpuList(value: unknown, client: GpuClient): readonly Gpu[] {
  if (!Array.isArray(value)) {
    throw new GpuNativeDataError("gpus", "expected an array");
  }
  if (value.length > MAX_GPU_DESCRIPTORS) {
    throw new GpuNativeDataError(
      "gpus",
      `array exceeds the ${String(MAX_GPU_DESCRIPTORS)}-entry safety limit`,
    );
  }
  const ids = new Set<string>();
  const gpus = value.map((rawDescriptor, index) => {
    if (!isRecord(rawDescriptor)) {
      throw new GpuNativeDataError(
        `gpus[${String(index)}]`,
        "expected an object",
      );
    }
    const hasIdentityEnvelope = rawDescriptor.identity !== undefined;
    const identity = parseGpuIdentity(
      hasIdentityEnvelope ? rawDescriptor.identity : rawDescriptor,
      `gpus[${String(index)}].identity`,
    );
    if (ids.has(identity.id)) {
      throw new GpuNativeDataError(
        `gpus[${String(index)}].identity.id`,
        `duplicate stable device id ${JSON.stringify(identity.id)}`,
      );
    }
    ids.add(identity.id);
    const capabilities = parseGpuCapabilities(
      rawDescriptor.capabilities,
      `gpus[${String(index)}].capabilities`,
    );
    return createGpu(client, identity, capabilities);
  });
  return Object.freeze(gpus);
}

export class GpuMonitor {
  readonly #native: NativeMonitorHandle;
  readonly #client: GpuClient;
  readonly #subscriptions = new Set<ManagedSubscription>();
  #state: MonitorState = "open";
  #gpuCache: Promise<readonly Gpu[]> | undefined;
  #closePromise: Promise<void> | undefined;

  private constructor(native: NativeMonitorHandle) {
    this.#native = native;
    this.#client = {
      assertOpen: () => this.#assertOpen(),
      sample: (id, options) => this.#sample(id, options),
      subscribe: (id, options) => this.#subscribe(id, options),
      vendorInfo: (id, vendor) => this.#vendorInfo(id, vendor),
    };
  }

  static async open(options?: MonitorOpenOptions): Promise<GpuMonitor> {
    const binding = await getNativeBinding();
    const native = assertNativeMonitor(
      await binding.openMonitor(normalizeOpenOptions(options)),
    );
    return new GpuMonitor(native);
  }

  #assertOpen(): void {
    if (this.#state !== "open") throw new GpuMonitorClosedError();
  }

  async gpus(): Promise<readonly Gpu[]> {
    this.#assertOpen();
    if (this.#gpuCache === undefined) {
      const pending = Promise.resolve(this.#native.listGpus()).then((value) => {
        this.#assertOpen();
        return parseGpuList(value, this.#client);
      });
      this.#gpuCache = pending;
      void pending.catch(() => {
        if (this.#gpuCache === pending) this.#gpuCache = undefined;
      });
    }
    return Object.freeze([...(await this.#gpuCache)]);
  }

  async #sample(
    id: string,
    options: {
      readonly windowMs?: number;
      readonly includeProcesses?: boolean;
    },
  ): Promise<GpuSnapshot> {
    this.#assertOpen();
    const value = await this.#native.sampleGpu(id, options);
    this.#assertOpen();
    return parseGpuSnapshot(value);
  }

  async #subscribe(
    id: string,
    options: {
      readonly intervalMs?: number;
      readonly includeProcesses?: boolean;
    },
  ): Promise<GpuSubscription> {
    this.#assertOpen();
    const rawSubscription = await this.#native.subscribeGpu(id, options);
    const native = assertNativeSubscription(rawSubscription);
    if (this.#state !== "open") {
      await Promise.resolve(native.cancel()).catch(() => undefined);
      throw new GpuMonitorClosedError();
    }
    const subscription = new ManagedSubscription(native, (finished) => {
      this.#subscriptions.delete(finished);
    });
    this.#subscriptions.add(subscription);
    return subscription;
  }

  async #vendorInfo(
    id: string,
    vendor: GpuVendor,
  ): Promise<Readonly<Record<string, unknown>>> {
    this.#assertOpen();
    const value = await this.#native.vendorInfo(id);
    this.#assertOpen();
    return vendorInfoRecord(value, vendor);
  }

  async diagnostics(): Promise<GpuDiagnostics> {
    this.#assertOpen();
    const value = await this.#native.diagnostics();
    this.#assertOpen();
    return parseGpuDiagnostics(value);
  }

  async refresh(): Promise<readonly Gpu[]> {
    this.#assertOpen();
    await this.#native.refresh();
    this.#assertOpen();
    this.#gpuCache = undefined;
    return this.gpus();
  }

  close(): Promise<void> {
    this.#closePromise ??= this.#performClose();
    return this.#closePromise;
  }

  async #performClose(): Promise<void> {
    if (this.#state === "closed") return;
    this.#state = "closing";
    this.#gpuCache = undefined;
    try {
      const cancellations = [...this.#subscriptions].map((subscription) =>
        subscription.cancel(),
      );
      await Promise.allSettled(cancellations);
      await this.#native.close();
    } finally {
      this.#subscriptions.clear();
      this.#state = "closed";
    }
  }
}
