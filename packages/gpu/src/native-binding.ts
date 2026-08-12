import { GpuNativeDataError } from "./types.js";
import { isRecord } from "./validation.js";

export interface NativeGpuSubscription {
  next(): unknown;
  cancel(): Promise<void> | void;
}

export interface NativeMonitorHandle {
  listGpus(): unknown;
  sampleGpu(
    id: string,
    options: {
      readonly windowMs?: number;
      readonly includeProcesses?: boolean;
    },
  ): unknown;
  subscribeGpu(
    id: string,
    options: {
      readonly intervalMs?: number;
      readonly includeProcesses?: boolean;
    },
  ): unknown;
  vendorInfo(id: string): unknown;
  diagnostics(): unknown;
  refresh(): Promise<void> | void;
  close(): Promise<void> | void;
}

export interface NativeBinding {
  openMonitor(options?: {
    readonly requiredProviders?: readonly string[];
    readonly enableApplePrivateTelemetry?: boolean;
    readonly includeSoftwareAdapters?: boolean;
  }): unknown;
}

let testingBinding: NativeBinding | undefined;
let loadedBinding: Promise<NativeBinding> | undefined;

function hasMethod(value: Record<string, unknown>, name: string): boolean {
  return typeof value[name] === "function";
}

export function assertNativeBinding(value: unknown): NativeBinding {
  if (!isRecord(value) || !hasMethod(value, "openMonitor")) {
    throw new GpuNativeDataError(
      "nativeBinding",
      "expected an object with an openMonitor() method",
    );
  }
  return value as unknown as NativeBinding;
}

export function assertNativeMonitor(value: unknown): NativeMonitorHandle {
  const requiredMethods = [
    "listGpus",
    "sampleGpu",
    "subscribeGpu",
    "vendorInfo",
    "diagnostics",
    "refresh",
    "close",
  ] as const;

  if (!isRecord(value)) {
    throw new GpuNativeDataError("nativeMonitor", "expected an object");
  }
  for (const method of requiredMethods) {
    if (!hasMethod(value, method)) {
      throw new GpuNativeDataError(
        `nativeMonitor.${method}`,
        "expected a function",
      );
    }
  }
  return value as unknown as NativeMonitorHandle;
}

export function assertNativeSubscription(
  value: unknown,
): NativeGpuSubscription {
  if (!isRecord(value)) {
    throw new GpuNativeDataError("nativeSubscription", "expected an object");
  }
  for (const method of ["next", "cancel"] as const) {
    if (!hasMethod(value, method)) {
      throw new GpuNativeDataError(
        `nativeSubscription.${method}`,
        "expected a function",
      );
    }
  }
  return value as unknown as NativeGpuSubscription;
}

async function importPackagedBinding(): Promise<NativeBinding> {
  // Keep this dynamic so tsup does not inline the CommonJS native loader. The
  // same relative specifier resolves from src/ in tests and dist/ in builds.
  const loaderSpecifier = "../native.cjs";
  const namespace: unknown = await import(loaderSpecifier);
  if (isRecord(namespace)) {
    const defaultExport = namespace.default;
    if (defaultExport !== undefined) {
      return assertNativeBinding(defaultExport);
    }
  }
  return assertNativeBinding(namespace);
}

export function getNativeBinding(): Promise<NativeBinding> {
  if (testingBinding !== undefined) return Promise.resolve(testingBinding);
  loadedBinding ??= importPackagedBinding();
  return loadedBinding;
}

/**
 * Test-only dependency injection hook. This module is deliberately not
 * re-exported from the package entry point.
 */
export function setNativeBindingForTesting(
  binding: NativeBinding | undefined,
): void {
  testingBinding = binding;
  loadedBinding = undefined;
}
