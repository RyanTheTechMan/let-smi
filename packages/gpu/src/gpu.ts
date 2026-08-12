import type {
  AmdInfo,
  AppleInfo,
  GpuCapabilities,
  GpuIdentity,
  GpuMetricName,
  GpuSnapshot,
  GpuVendor,
  IntelInfo,
  NvidiaInfo,
  SampleOptions,
  WatchOptions,
} from "./types.js";
import { normalizeSampleOptions, normalizeWatchOptions } from "./validation.js";

export interface GpuSubscription {
  next(): Promise<GpuSnapshot | null>;
  cancel(): Promise<void>;
}

export interface GpuClient {
  assertOpen(): void;
  sample(
    id: string,
    options: {
      readonly windowMs?: number;
      readonly includeProcesses?: boolean;
    },
  ): Promise<GpuSnapshot>;
  subscribe(
    id: string,
    options: {
      readonly intervalMs?: number;
      readonly includeProcesses?: boolean;
    },
  ): Promise<GpuSubscription>;
  vendorInfo(
    id: string,
    vendor: GpuVendor,
  ): Promise<Readonly<Record<string, unknown>>>;
}

type IdentityFor<TVendor extends GpuVendor> = GpuIdentity & {
  readonly vendor: TVendor;
};

const GPU_CONSTRUCTOR = Symbol("let-smi.gpu-constructor");

class CancellationState {
  #aborted: boolean;

  constructor(initiallyAborted: boolean) {
    this.#aborted = initiallyAborted;
  }

  abort(): void {
    this.#aborted = true;
  }

  isAborted(): boolean {
    return this.#aborted;
  }
}

export abstract class GenericGpu<TVendor extends GpuVendor = GpuVendor> {
  abstract readonly vendor: TVendor;

  readonly id: string;
  readonly identity: IdentityFor<TVendor>;
  readonly capabilities: GpuCapabilities;

  readonly #client: GpuClient;

  protected constructor(
    client: GpuClient,
    identity: IdentityFor<TVendor>,
    capabilities: GpuCapabilities,
  ) {
    this.#client = client;
    this.id = identity.id;
    this.identity = identity;
    this.capabilities = capabilities;
  }

  sample(options?: SampleOptions): Promise<GpuSnapshot> {
    this.#client.assertOpen();
    return this.#client.sample(this.id, normalizeSampleOptions(options));
  }

  async *samples(
    options?: WatchOptions,
  ): AsyncGenerator<GpuSnapshot, void, void> {
    const { native, signal } = normalizeWatchOptions(options);
    this.#client.assertOpen();
    let subscription: GpuSubscription | undefined;
    const cancellation = new CancellationState(signal?.aborted ?? false);
    const handleAbort = (): void => {
      cancellation.abort();
      // Native cancellation is asynchronous, while AbortSignal listeners are
      // synchronous. The generator's finally block observes the same promise.
      void subscription?.cancel().catch(() => undefined);
    };
    signal?.addEventListener("abort", handleAbort, { once: true });

    try {
      if (cancellation.isAborted()) return;
      try {
        subscription = await this.#client.subscribe(this.id, native);
      } catch (error) {
        if (cancellation.isAborted()) return;
        throw error;
      }
      if (cancellation.isAborted()) return;

      while (!cancellation.isAborted()) {
        let snapshot: GpuSnapshot | null;
        try {
          snapshot = await subscription.next();
        } catch (error) {
          if (cancellation.isAborted()) return;
          throw error;
        }
        if (snapshot === null || cancellation.isAborted()) return;
        yield snapshot;
      }
    } finally {
      signal?.removeEventListener("abort", handleAbort);
      const cancellationResult = subscription?.cancel();
      if (cancellation.isAborted()) {
        await cancellationResult?.catch(() => undefined);
      } else {
        await cancellationResult;
      }
    }
  }

  supports(metric: GpuMetricName): boolean {
    return this.capabilities.metrics.includes(metric);
  }

  protected vendorInformation(): Promise<Readonly<Record<string, unknown>>> {
    this.#client.assertOpen();
    return this.#client.vendorInfo(this.id, this.vendor);
  }
}

export class NvidiaGpu extends GenericGpu<"nvidia"> {
  override readonly vendor = "nvidia" as const;

  private constructor(
    client: GpuClient,
    identity: IdentityFor<"nvidia">,
    capabilities: GpuCapabilities,
  ) {
    super(client, identity, capabilities);
  }

  static [GPU_CONSTRUCTOR](
    client: GpuClient,
    identity: IdentityFor<"nvidia">,
    capabilities: GpuCapabilities,
  ): NvidiaGpu {
    return new NvidiaGpu(client, identity, capabilities);
  }

  nvidiaInfo(): Promise<NvidiaInfo> {
    return this.vendorInformation();
  }
}

export class AmdGpu extends GenericGpu<"amd"> {
  override readonly vendor = "amd" as const;

  private constructor(
    client: GpuClient,
    identity: IdentityFor<"amd">,
    capabilities: GpuCapabilities,
  ) {
    super(client, identity, capabilities);
  }

  static [GPU_CONSTRUCTOR](
    client: GpuClient,
    identity: IdentityFor<"amd">,
    capabilities: GpuCapabilities,
  ): AmdGpu {
    return new AmdGpu(client, identity, capabilities);
  }

  amdInfo(): Promise<AmdInfo> {
    return this.vendorInformation();
  }
}

export class IntelGpu extends GenericGpu<"intel"> {
  override readonly vendor = "intel" as const;

  private constructor(
    client: GpuClient,
    identity: IdentityFor<"intel">,
    capabilities: GpuCapabilities,
  ) {
    super(client, identity, capabilities);
  }

  static [GPU_CONSTRUCTOR](
    client: GpuClient,
    identity: IdentityFor<"intel">,
    capabilities: GpuCapabilities,
  ): IntelGpu {
    return new IntelGpu(client, identity, capabilities);
  }

  intelInfo(): Promise<IntelInfo> {
    return this.vendorInformation();
  }
}

export class AppleGpu extends GenericGpu<"apple"> {
  override readonly vendor = "apple" as const;

  private constructor(
    client: GpuClient,
    identity: IdentityFor<"apple">,
    capabilities: GpuCapabilities,
  ) {
    super(client, identity, capabilities);
  }

  static [GPU_CONSTRUCTOR](
    client: GpuClient,
    identity: IdentityFor<"apple">,
    capabilities: GpuCapabilities,
  ): AppleGpu {
    return new AppleGpu(client, identity, capabilities);
  }

  appleInfo(): Promise<AppleInfo> {
    return this.vendorInformation();
  }
}

export class UnknownGpu extends GenericGpu<"unknown"> {
  override readonly vendor = "unknown" as const;

  private constructor(
    client: GpuClient,
    identity: IdentityFor<"unknown">,
    capabilities: GpuCapabilities,
  ) {
    super(client, identity, capabilities);
  }

  static [GPU_CONSTRUCTOR](
    client: GpuClient,
    identity: IdentityFor<"unknown">,
    capabilities: GpuCapabilities,
  ): UnknownGpu {
    return new UnknownGpu(client, identity, capabilities);
  }
}

export type Gpu = NvidiaGpu | AmdGpu | IntelGpu | AppleGpu | UnknownGpu;

export function createGpu(
  client: GpuClient,
  identity: GpuIdentity,
  capabilities: GpuCapabilities,
): Gpu {
  switch (identity.vendor) {
    case "nvidia":
      return NvidiaGpu[GPU_CONSTRUCTOR](
        client,
        identity as IdentityFor<"nvidia">,
        capabilities,
      );
    case "amd":
      return AmdGpu[GPU_CONSTRUCTOR](
        client,
        identity as IdentityFor<"amd">,
        capabilities,
      );
    case "intel":
      return IntelGpu[GPU_CONSTRUCTOR](
        client,
        identity as IdentityFor<"intel">,
        capabilities,
      );
    case "apple":
      return AppleGpu[GPU_CONSTRUCTOR](
        client,
        identity as IdentityFor<"apple">,
        capabilities,
      );
    case "unknown":
      return UnknownGpu[GPU_CONSTRUCTOR](
        client,
        identity as IdentityFor<"unknown">,
        capabilities,
      );
  }
}
