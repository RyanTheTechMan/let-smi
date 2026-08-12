import type {
  GpuCapabilities,
  GpuClockSnapshot,
  GpuDiagnostics,
  GpuFanSnapshot,
  GpuIdentity,
  GpuKind,
  GpuMemorySnapshot,
  GpuMemoryTopology,
  MetricSelectionCandidate,
  MetricSelectionDiagnostics,
  GpuMetricName,
  GpuPartitionType,
  GpuPowerSnapshot,
  GpuProcessSnapshot,
  GpuProcessUtilizationSnapshot,
  GpuSnapshot,
  GpuTemperatureSnapshot,
  GpuUtilizationCapabilities,
  GpuUtilizationSnapshot,
  GpuVendor,
  Metric,
  MetricQuality,
  MonitorOpenOptions,
  ProviderDiagnostics,
  SampleOptions,
  UnavailableReason,
  WatchOptions,
} from "./types.js";
import { GpuInvalidArgumentError, GpuNativeDataError } from "./types.js";

const GPU_VENDORS = new Set<GpuVendor>([
  "nvidia",
  "amd",
  "intel",
  "apple",
  "unknown",
]);
const GPU_KINDS = new Set<GpuKind>([
  "integrated",
  "discrete",
  "external",
  "virtual",
  "unknown",
]);
const PARTITION_TYPES = new Set<GpuPartitionType>([
  "mig",
  "vgpu",
  "sriov",
  "tile",
]);
const MEMORY_TOPOLOGIES = new Set<GpuMemoryTopology>([
  "dedicated",
  "shared",
  "unified",
  "mixed",
  "unknown",
]);
const METRIC_QUALITIES = new Set<MetricQuality>([
  "direct",
  "derived",
  "estimated",
]);
const UNAVAILABLE_REASONS = new Set<UnavailableReason>([
  "unsupported",
  "driver-library-missing",
  "permission-denied",
  "device-lost",
  "first-sample",
  "temporarily-unavailable",
  "provider-error",
]);

export const GPU_METRIC_NAMES = [
  "utilization.overall",
  "utilization.graphics",
  "utilization.compute",
  "utilization.copy",
  "utilization.memoryController",
  "utilization.encoder",
  "utilization.decoder",
  "memory.dedicatedUsedBytes",
  "memory.sharedUsedBytes",
  "memory.unifiedUsedBytes",
  "memory.budgetBytes",
  "memory.bandwidthUtilizationPercent",
  "temperatures.coreCelsius",
  "temperatures.edgeCelsius",
  "temperatures.hotspotCelsius",
  "temperatures.memoryCelsius",
  "power.drawWatts",
  "power.limitWatts",
  "power.energyJoules",
  "clocks.graphicsMHz",
  "clocks.computeMHz",
  "clocks.memoryMHz",
  "clocks.videoMHz",
  "fan.percent",
  "fan.rpm",
  "processes",
] as const satisfies readonly GpuMetricName[];

const GPU_METRIC_NAME_SET = new Set<string>(GPU_METRIC_NAMES);
const MAX_NATIVE_STRING_LENGTH = 65_536;
const MAX_PROCESSES = 16_384;
const MAX_PROVIDER_DIAGNOSTICS = 32;
const MAX_DIAGNOSTIC_WARNINGS = 128;
const MAX_METRIC_SELECTIONS = 16_384;
const MAX_SELECTION_CANDIDATES = 32;
const MAX_REQUIRED_PROVIDERS = 16;
const MAX_PROVIDER_ID_LENGTH = 64;
const MAX_RECORD_KEYS = 256;

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function record(value: unknown, path: string): Record<string, unknown> {
  if (!isRecord(value)) {
    throw new GpuNativeDataError(path, "expected an object");
  }
  const keys = Object.keys(value);
  if (keys.length > MAX_RECORD_KEYS) {
    throw new GpuNativeDataError(
      path,
      `object exceeds the ${String(MAX_RECORD_KEYS)}-property safety limit`,
    );
  }
  return copyOwnProperties(value, keys);
}

function copyOwnProperties(
  value: Record<string, unknown>,
  keys = Object.keys(value),
): Record<string, unknown> {
  const result = Object.create(null) as Record<string, unknown>;
  for (const key of keys) result[key] = value[key];
  return result;
}

function nonEmptyString(value: unknown, path: string): string {
  if (
    typeof value !== "string" ||
    value.trim().length === 0 ||
    value.length > MAX_NATIVE_STRING_LENGTH
  ) {
    throw new GpuNativeDataError(path, "expected a non-empty string");
  }
  return value;
}

function optionalString(
  value: unknown,
  path: string,
  options: { readonly allowEmpty?: boolean } = {},
): string | undefined {
  if (value === undefined) return undefined;
  if (
    typeof value !== "string" ||
    value.length > MAX_NATIVE_STRING_LENGTH ||
    (!options.allowEmpty && value.trim().length === 0)
  ) {
    throw new GpuNativeDataError(path, "expected a string");
  }
  return value;
}

function finiteNumber(
  value: unknown,
  path: string,
  limits: {
    readonly min?: number;
    readonly max?: number;
    readonly integer?: boolean;
  } = {},
): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new GpuNativeDataError(path, "expected a finite number");
  }
  if (limits.integer && !Number.isSafeInteger(value)) {
    throw new GpuNativeDataError(path, "expected a safe integer");
  }
  if (limits.min !== undefined && value < limits.min) {
    throw new GpuNativeDataError(
      path,
      `expected a value greater than or equal to ${String(limits.min)}`,
    );
  }
  if (limits.max !== undefined && value > limits.max) {
    throw new GpuNativeDataError(
      path,
      `expected a value less than or equal to ${String(limits.max)}`,
    );
  }
  return value;
}

function optionalFiniteNumber(
  value: unknown,
  path: string,
  limits: {
    readonly min?: number;
    readonly max?: number;
    readonly integer?: boolean;
  } = {},
): number | undefined {
  return value === undefined ? undefined : finiteNumber(value, path, limits);
}

function boolean(value: unknown, path: string): boolean {
  if (typeof value !== "boolean") {
    throw new GpuNativeDataError(path, "expected a boolean");
  }
  return value;
}

function stringArray(value: unknown, path: string): readonly string[] {
  const values = boundedArray(value, path, 256);
  return Object.freeze(
    values.map((entry, index) =>
      nonEmptyString(entry, `${path}[${String(index)}]`),
    ),
  );
}

function boundedArray(
  value: unknown,
  path: string,
  maximumLength: number,
): readonly unknown[] {
  if (!Array.isArray(value)) {
    throw new GpuNativeDataError(path, "expected an array");
  }
  if (value.length > maximumLength) {
    throw new GpuNativeDataError(
      path,
      `array exceeds the ${String(maximumLength)}-entry safety limit`,
    );
  }
  return value;
}

function optionalObject(
  value: unknown,
  path: string,
): Record<string, unknown> | undefined {
  return value === undefined ? undefined : record(value, path);
}

export function parseGpuIdentity(
  value: unknown,
  path = "identity",
): GpuIdentity {
  const input = record(value, path);
  const vendor = nonEmptyString(input.vendor, `${path}.vendor`);
  if (!GPU_VENDORS.has(vendor as GpuVendor)) {
    throw new GpuNativeDataError(
      `${path}.vendor`,
      `unknown vendor ${JSON.stringify(vendor)}`,
    );
  }

  const kind = nonEmptyString(input.kind, `${path}.kind`);
  if (!GPU_KINDS.has(kind as GpuKind)) {
    throw new GpuNativeDataError(
      `${path}.kind`,
      `unknown GPU kind ${JSON.stringify(kind)}`,
    );
  }

  const pciInput = optionalObject(input.pci, `${path}.pci`);
  const windowsInput = optionalObject(input.windows, `${path}.windows`);
  const macosInput = optionalObject(input.macos, `${path}.macos`);
  const partitionInput = optionalObject(input.partition, `${path}.partition`);

  let pci: GpuIdentity["pci"];
  if (pciInput !== undefined) {
    const address = optionalString(pciInput.address, `${path}.pci.address`);
    const subsystemVendorId = optionalFiniteNumber(
      pciInput.subsystemVendorId,
      `${path}.pci.subsystemVendorId`,
      { min: 0, max: 0xffff, integer: true },
    );
    const subsystemDeviceId = optionalFiniteNumber(
      pciInput.subsystemDeviceId,
      `${path}.pci.subsystemDeviceId`,
      { min: 0, max: 0xffff, integer: true },
    );
    pci = Object.freeze({
      vendorId: finiteNumber(pciInput.vendorId, `${path}.pci.vendorId`, {
        min: 0,
        max: 0xffff,
        integer: true,
      }),
      deviceId: finiteNumber(pciInput.deviceId, `${path}.pci.deviceId`, {
        min: 0,
        max: 0xffff,
        integer: true,
      }),
      ...(address === undefined ? {} : { address }),
      ...(subsystemVendorId === undefined ? {} : { subsystemVendorId }),
      ...(subsystemDeviceId === undefined ? {} : { subsystemDeviceId }),
    });
  }

  let windows: GpuIdentity["windows"];
  if (windowsInput !== undefined) {
    const luid = optionalString(windowsInput.luid, `${path}.windows.luid`);
    const pnpDeviceId = optionalString(
      windowsInput.pnpDeviceId,
      `${path}.windows.pnpDeviceId`,
    );
    if (luid === undefined && pnpDeviceId === undefined) {
      throw new GpuNativeDataError(
        `${path}.windows`,
        "expected at least one identifier",
      );
    }
    windows = Object.freeze({
      ...(luid === undefined ? {} : { luid }),
      ...(pnpDeviceId === undefined ? {} : { pnpDeviceId }),
    });
  }

  let macos: GpuIdentity["macos"];
  if (macosInput !== undefined) {
    const registryEntryId = optionalString(
      macosInput.registryEntryId,
      `${path}.macos.registryEntryId`,
    );
    const metalRegistryId = optionalString(
      macosInput.metalRegistryId,
      `${path}.macos.metalRegistryId`,
    );
    if (registryEntryId === undefined && metalRegistryId === undefined) {
      throw new GpuNativeDataError(
        `${path}.macos`,
        "expected at least one identifier",
      );
    }
    macos = Object.freeze({
      ...(registryEntryId === undefined ? {} : { registryEntryId }),
      ...(metalRegistryId === undefined ? {} : { metalRegistryId }),
    });
  }

  let partition: GpuIdentity["partition"];
  if (partitionInput !== undefined) {
    const partitionType = nonEmptyString(
      partitionInput.type,
      `${path}.partition.type`,
    );
    if (!PARTITION_TYPES.has(partitionType as GpuPartitionType)) {
      throw new GpuNativeDataError(
        `${path}.partition.type`,
        `unknown partition type ${JSON.stringify(partitionType)}`,
      );
    }
    partition = Object.freeze({
      type: partitionType as GpuPartitionType,
      id: nonEmptyString(partitionInput.id, `${path}.partition.id`),
    });
  }

  const architecture = optionalString(
    input.architecture,
    `${path}.architecture`,
  );
  const driverVersion = optionalString(
    input.driverVersion,
    `${path}.driverVersion`,
  );
  const firmwareVersion = optionalString(
    input.firmwareVersion,
    `${path}.firmwareVersion`,
  );
  const uuid = optionalString(input.uuid, `${path}.uuid`);
  const parentDeviceId = optionalString(
    input.parentDeviceId,
    `${path}.parentDeviceId`,
  );

  return Object.freeze({
    id: nonEmptyString(input.id, `${path}.id`),
    vendor: vendor as GpuVendor,
    name: nonEmptyString(input.name, `${path}.name`),
    kind: kind as GpuKind,
    ...(architecture === undefined ? {} : { architecture }),
    ...(driverVersion === undefined ? {} : { driverVersion }),
    ...(firmwareVersion === undefined ? {} : { firmwareVersion }),
    ...(uuid === undefined ? {} : { uuid }),
    ...(pci === undefined ? {} : { pci }),
    ...(windows === undefined ? {} : { windows }),
    ...(macos === undefined ? {} : { macos }),
    ...(parentDeviceId === undefined ? {} : { parentDeviceId }),
    ...(partition === undefined ? {} : { partition }),
  });
}

function inferredMetric(
  metrics: ReadonlySet<string>,
  name: GpuMetricName,
): boolean {
  return metrics.has(name);
}

function optionalCapabilityBoolean(
  input: Record<string, unknown> | undefined,
  key: string,
  path: string,
  fallback: boolean,
): boolean {
  const value = input?.[key];
  return value === undefined ? fallback : boolean(value, `${path}.${key}`);
}

export function parseGpuCapabilities(
  value: unknown,
  path = "capabilities",
): GpuCapabilities {
  if (value === undefined) {
    const utilization: GpuUtilizationCapabilities = Object.freeze({
      overall: false,
      graphics: false,
      compute: false,
      copy: false,
      memoryController: false,
      encoder: false,
      decoder: false,
    });
    return Object.freeze({
      metrics: Object.freeze([]),
      utilization,
      temperature: false,
      power: false,
      clocks: false,
      fan: false,
      processes: false,
      vendorExtensions: Object.freeze([]),
    });
  }

  const input = record(value, path);
  const rawMetrics = input.metrics ?? [];
  const metricValues = boundedArray(
    rawMetrics,
    `${path}.metrics`,
    GPU_METRIC_NAMES.length,
  );
  const metrics: GpuMetricName[] = [];
  const seen = new Set<string>();
  for (const [index, rawMetric] of metricValues.entries()) {
    const metric = nonEmptyString(
      rawMetric,
      `${path}.metrics[${String(index)}]`,
    );
    if (!GPU_METRIC_NAME_SET.has(metric)) {
      throw new GpuNativeDataError(
        `${path}.metrics[${String(index)}]`,
        `unknown metric ${metric}`,
      );
    }
    if (!seen.has(metric)) {
      seen.add(metric);
      metrics.push(metric as GpuMetricName);
    }
  }
  const metricSet: ReadonlySet<string> = seen;
  const utilizationInput = optionalObject(
    input.utilization,
    `${path}.utilization`,
  );
  const utilization: GpuUtilizationCapabilities = Object.freeze({
    overall: optionalCapabilityBoolean(
      utilizationInput,
      "overall",
      `${path}.utilization`,
      inferredMetric(metricSet, "utilization.overall"),
    ),
    graphics: optionalCapabilityBoolean(
      utilizationInput,
      "graphics",
      `${path}.utilization`,
      inferredMetric(metricSet, "utilization.graphics"),
    ),
    compute: optionalCapabilityBoolean(
      utilizationInput,
      "compute",
      `${path}.utilization`,
      inferredMetric(metricSet, "utilization.compute"),
    ),
    copy: optionalCapabilityBoolean(
      utilizationInput,
      "copy",
      `${path}.utilization`,
      inferredMetric(metricSet, "utilization.copy"),
    ),
    memoryController: optionalCapabilityBoolean(
      utilizationInput,
      "memoryController",
      `${path}.utilization`,
      inferredMetric(metricSet, "utilization.memoryController"),
    ),
    encoder: optionalCapabilityBoolean(
      utilizationInput,
      "encoder",
      `${path}.utilization`,
      inferredMetric(metricSet, "utilization.encoder"),
    ),
    decoder: optionalCapabilityBoolean(
      utilizationInput,
      "decoder",
      `${path}.utilization`,
      inferredMetric(metricSet, "utilization.decoder"),
    ),
  });

  const hasPrefix = (prefix: string): boolean =>
    metrics.some((metric) => metric.startsWith(prefix));
  const vendorExtensions =
    input.vendorExtensions === undefined
      ? (Object.freeze([]) as readonly string[])
      : stringArray(input.vendorExtensions, `${path}.vendorExtensions`);

  return Object.freeze({
    metrics: Object.freeze(metrics),
    utilization,
    temperature:
      input.temperature === undefined
        ? hasPrefix("temperatures.")
        : boolean(input.temperature, `${path}.temperature`),
    power:
      input.power === undefined
        ? hasPrefix("power.")
        : boolean(input.power, `${path}.power`),
    clocks:
      input.clocks === undefined
        ? hasPrefix("clocks.")
        : boolean(input.clocks, `${path}.clocks`),
    fan:
      input.fan === undefined
        ? hasPrefix("fan.")
        : boolean(input.fan, `${path}.fan`),
    processes:
      input.processes === undefined
        ? inferredMetric(metricSet, "processes")
        : boolean(input.processes, `${path}.processes`),
    vendorExtensions,
  });
}

function parseMetricNumber(
  value: unknown,
  path: string,
  limits: {
    readonly min?: number;
    readonly max?: number;
    readonly integer?: boolean;
  } = {},
): Metric<number> {
  const input = record(value, path);
  if (input.available === true) {
    const quality = nonEmptyString(input.quality, `${path}.quality`);
    if (!METRIC_QUALITIES.has(quality as MetricQuality)) {
      throw new GpuNativeDataError(
        `${path}.quality`,
        `unknown quality ${quality}`,
      );
    }
    const intervalMs = optionalFiniteNumber(
      input.intervalMs,
      `${path}.intervalMs`,
      {
        min: 0,
      },
    );
    const definition = optionalString(input.definition, `${path}.definition`, {
      allowEmpty: true,
    });
    return Object.freeze({
      available: true,
      value: finiteNumber(input.value, `${path}.value`, limits),
      source: nonEmptyString(input.source, `${path}.source`),
      quality: quality as MetricQuality,
      sampledAt: finiteNumber(input.sampledAt, `${path}.sampledAt`, { min: 0 }),
      ...(intervalMs === undefined ? {} : { intervalMs }),
      ...(definition === undefined ? {} : { definition }),
    });
  }

  if (input.available === false) {
    const reason = nonEmptyString(input.reason, `${path}.reason`);
    if (!UNAVAILABLE_REASONS.has(reason as UnavailableReason)) {
      throw new GpuNativeDataError(
        `${path}.reason`,
        `unknown unavailable reason ${reason}`,
      );
    }
    const source = optionalString(input.source, `${path}.source`);
    const message = optionalString(input.message, `${path}.message`, {
      allowEmpty: true,
    });
    return Object.freeze({
      available: false,
      reason: reason as UnavailableReason,
      ...(source === undefined ? {} : { source }),
      ...(message === undefined ? {} : { message }),
    });
  }

  throw new GpuNativeDataError(`${path}.available`, "expected true or false");
}

function optionalMetric(
  input: Record<string, unknown>,
  key: string,
  path: string,
  limits: {
    readonly min?: number;
    readonly max?: number;
    readonly integer?: boolean;
  } = {},
): Metric<number> | undefined {
  return input[key] === undefined
    ? undefined
    : parseMetricNumber(input[key], `${path}.${key}`, limits);
}

function parseUtilization(
  value: unknown,
  path: string,
): GpuUtilizationSnapshot {
  const input = record(value, path);
  const graphics = optionalMetric(input, "graphics", path, {
    min: 0,
    max: 100,
  });
  const compute = optionalMetric(input, "compute", path, { min: 0, max: 100 });
  const copy = optionalMetric(input, "copy", path, { min: 0, max: 100 });
  const memoryController = optionalMetric(input, "memoryController", path, {
    min: 0,
    max: 100,
  });
  const encoder = optionalMetric(input, "encoder", path, { min: 0, max: 100 });
  const decoder = optionalMetric(input, "decoder", path, { min: 0, max: 100 });
  return Object.freeze({
    overall: parseMetricNumber(input.overall, `${path}.overall`, {
      min: 0,
      max: 100,
    }),
    ...(graphics === undefined ? {} : { graphics }),
    ...(compute === undefined ? {} : { compute }),
    ...(copy === undefined ? {} : { copy }),
    ...(memoryController === undefined ? {} : { memoryController }),
    ...(encoder === undefined ? {} : { encoder }),
    ...(decoder === undefined ? {} : { decoder }),
  });
}

function parseMemory(value: unknown, path: string): GpuMemorySnapshot {
  const input = record(value, path);
  const topology = nonEmptyString(input.topology, `${path}.topology`);
  if (!MEMORY_TOPOLOGIES.has(topology as GpuMemoryTopology)) {
    throw new GpuNativeDataError(
      `${path}.topology`,
      `unknown topology ${topology}`,
    );
  }
  const byteLimits = { min: 0, integer: true } as const;
  const dedicatedTotalBytes = optionalFiniteNumber(
    input.dedicatedTotalBytes,
    `${path}.dedicatedTotalBytes`,
    byteLimits,
  );
  const sharedTotalBytes = optionalFiniteNumber(
    input.sharedTotalBytes,
    `${path}.sharedTotalBytes`,
    byteLimits,
  );
  const unifiedTotalBytes = optionalFiniteNumber(
    input.unifiedTotalBytes,
    `${path}.unifiedTotalBytes`,
    byteLimits,
  );
  const dedicatedUsedBytes = optionalMetric(
    input,
    "dedicatedUsedBytes",
    path,
    byteLimits,
  );
  const sharedUsedBytes = optionalMetric(
    input,
    "sharedUsedBytes",
    path,
    byteLimits,
  );
  const unifiedUsedBytes = optionalMetric(
    input,
    "unifiedUsedBytes",
    path,
    byteLimits,
  );
  const budgetBytes = optionalMetric(input, "budgetBytes", path, byteLimits);
  const bandwidthUtilizationPercent = optionalMetric(
    input,
    "bandwidthUtilizationPercent",
    path,
    { min: 0, max: 100 },
  );
  return Object.freeze({
    topology: topology as GpuMemoryTopology,
    ...(dedicatedTotalBytes === undefined ? {} : { dedicatedTotalBytes }),
    ...(dedicatedUsedBytes === undefined ? {} : { dedicatedUsedBytes }),
    ...(sharedTotalBytes === undefined ? {} : { sharedTotalBytes }),
    ...(sharedUsedBytes === undefined ? {} : { sharedUsedBytes }),
    ...(unifiedTotalBytes === undefined ? {} : { unifiedTotalBytes }),
    ...(unifiedUsedBytes === undefined ? {} : { unifiedUsedBytes }),
    ...(budgetBytes === undefined ? {} : { budgetBytes }),
    ...(bandwidthUtilizationPercent === undefined
      ? {}
      : { bandwidthUtilizationPercent }),
  });
}

function parseTemperatures(
  value: unknown,
  path: string,
): GpuTemperatureSnapshot {
  const input = record(value, path);
  const coreCelsius = optionalMetric(input, "coreCelsius", path);
  const edgeCelsius = optionalMetric(input, "edgeCelsius", path);
  const hotspotCelsius = optionalMetric(input, "hotspotCelsius", path);
  const memoryCelsius = optionalMetric(input, "memoryCelsius", path);
  return Object.freeze({
    ...(coreCelsius === undefined ? {} : { coreCelsius }),
    ...(edgeCelsius === undefined ? {} : { edgeCelsius }),
    ...(hotspotCelsius === undefined ? {} : { hotspotCelsius }),
    ...(memoryCelsius === undefined ? {} : { memoryCelsius }),
  });
}

function parsePower(value: unknown, path: string): GpuPowerSnapshot {
  const input = record(value, path);
  const drawWatts = optionalMetric(input, "drawWatts", path, { min: 0 });
  const limitWatts = optionalMetric(input, "limitWatts", path, { min: 0 });
  const energyJoules = optionalMetric(input, "energyJoules", path, { min: 0 });
  return Object.freeze({
    ...(drawWatts === undefined ? {} : { drawWatts }),
    ...(limitWatts === undefined ? {} : { limitWatts }),
    ...(energyJoules === undefined ? {} : { energyJoules }),
  });
}

function parseClocks(value: unknown, path: string): GpuClockSnapshot {
  const input = record(value, path);
  const graphicsMHz = optionalMetric(input, "graphicsMHz", path, { min: 0 });
  const computeMHz = optionalMetric(input, "computeMHz", path, { min: 0 });
  const memoryMHz = optionalMetric(input, "memoryMHz", path, { min: 0 });
  const videoMHz = optionalMetric(input, "videoMHz", path, { min: 0 });
  return Object.freeze({
    ...(graphicsMHz === undefined ? {} : { graphicsMHz }),
    ...(computeMHz === undefined ? {} : { computeMHz }),
    ...(memoryMHz === undefined ? {} : { memoryMHz }),
    ...(videoMHz === undefined ? {} : { videoMHz }),
  });
}

function parseFan(value: unknown, path: string): GpuFanSnapshot {
  const input = record(value, path);
  const percent = optionalMetric(input, "percent", path, { min: 0, max: 100 });
  const rpm = optionalMetric(input, "rpm", path, { min: 0 });
  return Object.freeze({
    ...(percent === undefined ? {} : { percent }),
    ...(rpm === undefined ? {} : { rpm }),
  });
}

function parseProcessUtilization(
  value: unknown,
  path: string,
): GpuProcessUtilizationSnapshot {
  const input = record(value, path);
  const limits = { min: 0, max: 100 } as const;
  const overall = optionalMetric(input, "overall", path, limits);
  const graphics = optionalMetric(input, "graphics", path, limits);
  const compute = optionalMetric(input, "compute", path, limits);
  const encoder = optionalMetric(input, "encoder", path, limits);
  const decoder = optionalMetric(input, "decoder", path, limits);
  return Object.freeze({
    ...(overall === undefined ? {} : { overall }),
    ...(graphics === undefined ? {} : { graphics }),
    ...(compute === undefined ? {} : { compute }),
    ...(encoder === undefined ? {} : { encoder }),
    ...(decoder === undefined ? {} : { decoder }),
  });
}

function parseProcess(value: unknown, path: string): GpuProcessSnapshot {
  const input = record(value, path);
  const name = optionalString(input.name, `${path}.name`);
  const memoryUsedBytes =
    input.memoryUsedBytes === undefined
      ? undefined
      : parseMetricNumber(input.memoryUsedBytes, `${path}.memoryUsedBytes`, {
          min: 0,
          integer: true,
        });
  const utilization =
    input.utilization === undefined
      ? undefined
      : parseProcessUtilization(input.utilization, `${path}.utilization`);
  return Object.freeze({
    pid: finiteNumber(input.pid, `${path}.pid`, { min: 0, integer: true }),
    ...(name === undefined ? {} : { name }),
    ...(memoryUsedBytes === undefined ? {} : { memoryUsedBytes }),
    ...(utilization === undefined ? {} : { utilization }),
  });
}

export function parseGpuSnapshot(
  value: unknown,
  path = "snapshot",
): GpuSnapshot {
  const input = record(value, path);
  let processes: readonly GpuProcessSnapshot[] | undefined;
  if (input.processes !== undefined) {
    const processValues = boundedArray(
      input.processes,
      `${path}.processes`,
      MAX_PROCESSES,
    );
    processes = Object.freeze(
      processValues.map((process, index) =>
        parseProcess(process, `${path}.processes[${String(index)}]`),
      ),
    );
  }
  return Object.freeze({
    sampledAt: finiteNumber(input.sampledAt, `${path}.sampledAt`, { min: 0 }),
    utilization: parseUtilization(input.utilization, `${path}.utilization`),
    memory: parseMemory(input.memory, `${path}.memory`),
    temperatures: parseTemperatures(input.temperatures, `${path}.temperatures`),
    power: parsePower(input.power, `${path}.power`),
    clocks: parseClocks(input.clocks, `${path}.clocks`),
    fan: parseFan(input.fan, `${path}.fan`),
    ...(processes === undefined ? {} : { processes }),
  });
}

function parseProviderDiagnostics(
  value: unknown,
  path: string,
): ProviderDiagnostics {
  const input = record(value, path);
  const version = optionalString(input.version, `${path}.version`, {
    allowEmpty: true,
  });
  const devicesMatched = optionalFiniteNumber(
    input.devicesMatched,
    `${path}.devicesMatched`,
    {
      min: 0,
      integer: true,
    },
  );
  const reasonString = optionalString(input.reason, `${path}.reason`);
  let reason: UnavailableReason | undefined;
  if (reasonString !== undefined) {
    if (!UNAVAILABLE_REASONS.has(reasonString as UnavailableReason)) {
      throw new GpuNativeDataError(
        `${path}.reason`,
        `unknown reason ${reasonString}`,
      );
    }
    reason = reasonString as UnavailableReason;
  }
  const message = optionalString(input.message, `${path}.message`, {
    allowEmpty: true,
  });
  return Object.freeze({
    id: nonEmptyString(input.id, `${path}.id`),
    loaded: boolean(input.loaded, `${path}.loaded`),
    ...(version === undefined ? {} : { version }),
    ...(devicesMatched === undefined ? {} : { devicesMatched }),
    ...(reason === undefined ? {} : { reason }),
    ...(message === undefined ? {} : { message }),
  });
}

function parseMetricSelectionCandidate(
  value: unknown,
  path: string,
): MetricSelectionCandidate {
  const input = record(value, path);
  const quality = nonEmptyString(input.quality, `${path}.quality`);
  if (!METRIC_QUALITIES.has(quality as MetricQuality)) {
    throw new GpuNativeDataError(
      `${path}.quality`,
      `unknown quality ${quality}`,
    );
  }
  return Object.freeze({
    source: nonEmptyString(input.source, `${path}.source`),
    score: finiteNumber(input.score, `${path}.score`),
    selected: boolean(input.selected, `${path}.selected`),
    quality: quality as MetricQuality,
    sampledAt: finiteNumber(input.sampledAt, `${path}.sampledAt`, { min: 0 }),
  });
}

function parseMetricSelection(
  value: unknown,
  deviceId: string,
  path: string,
): MetricSelectionDiagnostics {
  const input = record(value, path);
  const metric = nonEmptyString(input.metric, `${path}.metric`);
  if (!GPU_METRIC_NAME_SET.has(metric)) {
    throw new GpuNativeDataError(`${path}.metric`, `unknown metric ${metric}`);
  }
  const candidates = boundedArray(
    input.candidates,
    `${path}.candidates`,
    MAX_SELECTION_CANDIDATES,
  );
  return Object.freeze({
    deviceId,
    metric: metric as GpuMetricName,
    candidates: Object.freeze(
      candidates.map((candidate, index) =>
        parseMetricSelectionCandidate(
          candidate,
          `${path}.candidates[${String(index)}]`,
        ),
      ),
    ),
  });
}

export function parseGpuDiagnostics(
  value: unknown,
  path = "diagnostics",
): GpuDiagnostics {
  const input = record(value, path);
  const providers = boundedArray(
    input.providers,
    `${path}.providers`,
    MAX_PROVIDER_DIAGNOSTICS,
  );
  const warnings =
    input.warnings === undefined
      ? Object.freeze([])
      : Object.freeze(
          boundedArray(
            input.warnings,
            `${path}.warnings`,
            MAX_DIAGNOSTIC_WARNINGS,
          ).map((warning, index) =>
            nonEmptyString(warning, `${path}.warnings[${String(index)}]`),
          ),
        );
  let metricSelections: readonly MetricSelectionDiagnostics[] | undefined;
  if (input.metricSelections !== undefined) {
    const selections = boundedArray(
      input.metricSelections,
      `${path}.metricSelections`,
      MAX_METRIC_SELECTIONS,
    );
    const flattenedSelections: MetricSelectionDiagnostics[] = [];
    const appendSelection = (selection: MetricSelectionDiagnostics): void => {
      if (flattenedSelections.length >= MAX_METRIC_SELECTIONS) {
        throw new GpuNativeDataError(
          `${path}.metricSelections`,
          `flattened selections exceed the ${String(MAX_METRIC_SELECTIONS)}-entry safety limit`,
        );
      }
      flattenedSelections.push(selection);
    };
    for (const [selectionIndex, selection] of selections.entries()) {
      const selectionPath = `${path}.metricSelections[${String(selectionIndex)}]`;
      const selectionInput = record(selection, selectionPath);
      const deviceId = nonEmptyString(
        selectionInput.deviceId,
        `${selectionPath}.deviceId`,
      );
      if (selectionInput.metrics === undefined) {
        appendSelection(
          parseMetricSelection(selectionInput, deviceId, selectionPath),
        );
        continue;
      }
      const metrics = boundedArray(
        selectionInput.metrics,
        `${selectionPath}.metrics`,
        GPU_METRIC_NAMES.length,
      );
      for (const [metricIndex, metric] of metrics.entries()) {
        appendSelection(
          parseMetricSelection(
            metric,
            deviceId,
            `${selectionPath}.metrics[${String(metricIndex)}]`,
          ),
        );
      }
    }
    metricSelections = Object.freeze(flattenedSelections);
  }
  return Object.freeze({
    platform: nonEmptyString(input.platform, `${path}.platform`),
    arch: nonEmptyString(input.arch, `${path}.arch`),
    providers: Object.freeze(
      providers.map((provider, index) =>
        parseProviderDiagnostics(
          provider,
          `${path}.providers[${String(index)}]`,
        ),
      ),
    ),
    warnings,
    ...(metricSelections === undefined ? {} : { metricSelections }),
  });
}

function positiveIntegerOption(
  value: unknown,
  name: string,
  minimum: number,
  maximum: number,
): number | undefined {
  if (value === undefined) return undefined;
  if (
    typeof value !== "number" ||
    !Number.isSafeInteger(value) ||
    value < minimum ||
    value > maximum
  ) {
    throw new GpuInvalidArgumentError(
      `${name} must be an integer between ${String(minimum)} and ${String(maximum)}`,
    );
  }
  return value;
}

function optionsRecord(
  value: unknown,
  name: string,
  allowedKeys: ReadonlySet<string>,
): Record<string, unknown> {
  if (value === undefined)
    return Object.create(null) as Record<string, unknown>;
  if (!isRecord(value)) {
    throw new GpuInvalidArgumentError(`${name} must be an object`);
  }
  const keys = Object.keys(value);
  const unknown = keys.find((key) => !allowedKeys.has(key));
  if (unknown !== undefined) {
    throw new GpuInvalidArgumentError(
      `${name} contains unknown option ${unknown}`,
    );
  }
  const input = copyOwnProperties(value, keys);
  return input;
}

export function normalizeOpenOptions(options: MonitorOpenOptions | undefined): {
  readonly requiredProviders?: string[];
  readonly enableApplePrivateTelemetry?: boolean;
  readonly includeSoftwareAdapters?: boolean;
} {
  const input = optionsRecord(
    options,
    "monitor options",
    new Set([
      "requiredProviders",
      "enableApplePrivateTelemetry",
      "includeSoftwareAdapters",
    ]),
  );
  const requiredProviders: string[] = [];
  if (input.requiredProviders !== undefined) {
    if (!Array.isArray(input.requiredProviders)) {
      throw new GpuInvalidArgumentError("requiredProviders must be an array");
    }
    if (input.requiredProviders.length > MAX_REQUIRED_PROVIDERS) {
      throw new GpuInvalidArgumentError(
        `requiredProviders exceeds the ${String(MAX_REQUIRED_PROVIDERS)}-entry safety limit`,
      );
    }
    const seen = new Set<string>();
    for (const provider of input.requiredProviders) {
      if (
        typeof provider !== "string" ||
        provider.length > MAX_PROVIDER_ID_LENGTH ||
        !/^[a-z0-9][a-z0-9._-]*$/u.test(provider)
      ) {
        throw new GpuInvalidArgumentError(
          "requiredProviders contains an invalid provider identifier",
        );
      }
      if (!seen.has(provider)) {
        seen.add(provider);
        requiredProviders.push(provider);
      }
    }
  }
  const enableApplePrivateTelemetry = optionalBooleanOption(
    input.enableApplePrivateTelemetry,
    "enableApplePrivateTelemetry",
  );
  const includeSoftwareAdapters = optionalBooleanOption(
    input.includeSoftwareAdapters,
    "includeSoftwareAdapters",
  );
  return {
    ...(input.requiredProviders === undefined ? {} : { requiredProviders }),
    ...(enableApplePrivateTelemetry === undefined
      ? {}
      : { enableApplePrivateTelemetry }),
    ...(includeSoftwareAdapters === undefined
      ? {}
      : { includeSoftwareAdapters }),
  };
}

function optionalBooleanOption(
  value: unknown,
  name: string,
): boolean | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "boolean") {
    throw new GpuInvalidArgumentError(`${name} must be a boolean`);
  }
  return value;
}

export function normalizeSampleOptions(options: SampleOptions | undefined): {
  readonly windowMs?: number;
  readonly includeProcesses?: boolean;
} {
  const input = optionsRecord(
    options,
    "sample options",
    new Set(["windowMs", "includeProcesses"]),
  );
  const windowMs = positiveIntegerOption(input.windowMs, "windowMs", 1, 60_000);
  const includeProcesses = optionalBooleanOption(
    input.includeProcesses,
    "includeProcesses",
  );
  return {
    ...(windowMs === undefined ? {} : { windowMs }),
    ...(includeProcesses === undefined ? {} : { includeProcesses }),
  };
}

export interface NormalizedWatchOptions {
  readonly native: {
    readonly intervalMs?: number;
    readonly includeProcesses?: boolean;
  };
  readonly signal?: AbortSignal;
}

function isAbortSignal(value: unknown): value is AbortSignal {
  if (typeof value !== "object" || value === null) return false;
  try {
    const candidate = value as Partial<AbortSignal>;
    return (
      typeof candidate.aborted === "boolean" &&
      typeof candidate.addEventListener === "function" &&
      typeof candidate.removeEventListener === "function"
    );
  } catch {
    return false;
  }
}

export function normalizeWatchOptions(
  options: WatchOptions | undefined,
): NormalizedWatchOptions {
  const input = optionsRecord(
    options,
    "watch options",
    new Set(["intervalMs", "includeProcesses", "signal"]),
  );
  const intervalMs = positiveIntegerOption(
    input.intervalMs,
    "intervalMs",
    50,
    60_000,
  );
  const includeProcesses = optionalBooleanOption(
    input.includeProcesses,
    "includeProcesses",
  );
  const signal = input.signal;
  if (signal !== undefined && !isAbortSignal(signal)) {
    throw new GpuInvalidArgumentError("signal must be an AbortSignal");
  }
  return {
    native: {
      ...(intervalMs === undefined ? {} : { intervalMs }),
      ...(includeProcesses === undefined ? {} : { includeProcesses }),
    },
    ...(signal === undefined ? {} : { signal }),
  };
}

export function vendorInfoRecord(
  value: unknown,
  expectedVendor: GpuVendor,
): Readonly<Record<string, unknown>> {
  const input = record(value, "vendorInfo");
  if (input.info === undefined) return Object.freeze({ ...input });
  const declaredVendor = optionalString(input.vendor, "vendorInfo.vendor");
  if (declaredVendor !== undefined && declaredVendor !== expectedVendor) {
    throw new GpuNativeDataError(
      "vendorInfo.vendor",
      `expected ${expectedVendor}, received ${declaredVendor}`,
    );
  }
  const info = record(input.info, "vendorInfo.info");
  return Object.freeze({ ...info });
}
