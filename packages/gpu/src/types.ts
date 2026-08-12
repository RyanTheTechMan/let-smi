export type GpuVendor = "nvidia" | "amd" | "intel" | "apple" | "unknown";

export type GpuKind =
  "integrated" | "discrete" | "external" | "virtual" | "unknown";

export type GpuPartitionType = "mig" | "vgpu" | "sriov" | "tile";

export interface GpuPciIdentity {
  readonly address?: string;
  readonly vendorId: number;
  readonly deviceId: number;
  readonly subsystemVendorId?: number;
  readonly subsystemDeviceId?: number;
}

export interface GpuWindowsIdentity {
  readonly luid?: string;
  readonly pnpDeviceId?: string;
}

export interface GpuMacosIdentity {
  readonly registryEntryId?: string;
  readonly metalRegistryId?: string;
}

export interface GpuPartitionIdentity {
  readonly type: GpuPartitionType;
  readonly id: string;
}

export interface GpuIdentity {
  readonly id: string;
  readonly vendor: GpuVendor;
  readonly name: string;
  readonly architecture?: string;
  readonly driverVersion?: string;
  readonly firmwareVersion?: string;
  readonly kind: GpuKind;
  readonly uuid?: string;
  readonly pci?: GpuPciIdentity;
  readonly windows?: GpuWindowsIdentity;
  readonly macos?: GpuMacosIdentity;
  readonly parentDeviceId?: string;
  readonly partition?: GpuPartitionIdentity;
}

export type UnavailableReason =
  | "unsupported"
  | "driver-library-missing"
  | "permission-denied"
  | "device-lost"
  | "first-sample"
  | "temporarily-unavailable"
  | "provider-error";

export type MetricQuality = "direct" | "derived" | "estimated";

export type Metric<T> =
  | {
      readonly available: true;
      readonly value: T;
      readonly source: string;
      readonly quality: MetricQuality;
      readonly sampledAt: number;
      readonly intervalMs?: number;
      readonly definition?: string;
    }
  | {
      readonly available: false;
      readonly reason: UnavailableReason;
      readonly source?: string;
      readonly message?: string;
    };

export type GpuMetricName =
  | "utilization.overall"
  | "utilization.graphics"
  | "utilization.compute"
  | "utilization.copy"
  | "utilization.memoryController"
  | "utilization.encoder"
  | "utilization.decoder"
  | "memory.dedicatedUsedBytes"
  | "memory.sharedUsedBytes"
  | "memory.unifiedUsedBytes"
  | "memory.budgetBytes"
  | "memory.bandwidthUtilizationPercent"
  | "temperatures.coreCelsius"
  | "temperatures.edgeCelsius"
  | "temperatures.hotspotCelsius"
  | "temperatures.memoryCelsius"
  | "power.drawWatts"
  | "power.limitWatts"
  | "power.energyJoules"
  | "clocks.graphicsMHz"
  | "clocks.computeMHz"
  | "clocks.memoryMHz"
  | "clocks.videoMHz"
  | "fan.percent"
  | "fan.rpm"
  | "processes";

export interface GpuUtilizationSnapshot {
  readonly overall: Metric<number>;
  readonly graphics?: Metric<number>;
  readonly compute?: Metric<number>;
  readonly copy?: Metric<number>;
  readonly memoryController?: Metric<number>;
  readonly encoder?: Metric<number>;
  readonly decoder?: Metric<number>;
}

export type GpuMemoryTopology =
  "dedicated" | "shared" | "unified" | "mixed" | "unknown";

export interface GpuMemorySnapshot {
  readonly topology: GpuMemoryTopology;
  readonly dedicatedTotalBytes?: number;
  readonly dedicatedUsedBytes?: Metric<number>;
  readonly sharedTotalBytes?: number;
  readonly sharedUsedBytes?: Metric<number>;
  readonly unifiedTotalBytes?: number;
  readonly unifiedUsedBytes?: Metric<number>;
  readonly budgetBytes?: Metric<number>;
  readonly bandwidthUtilizationPercent?: Metric<number>;
}

export interface GpuTemperatureSnapshot {
  readonly coreCelsius?: Metric<number>;
  readonly edgeCelsius?: Metric<number>;
  readonly hotspotCelsius?: Metric<number>;
  readonly memoryCelsius?: Metric<number>;
}

export interface GpuPowerSnapshot {
  readonly drawWatts?: Metric<number>;
  readonly limitWatts?: Metric<number>;
  readonly energyJoules?: Metric<number>;
}

export interface GpuClockSnapshot {
  readonly graphicsMHz?: Metric<number>;
  readonly computeMHz?: Metric<number>;
  readonly memoryMHz?: Metric<number>;
  readonly videoMHz?: Metric<number>;
}

export interface GpuFanSnapshot {
  readonly percent?: Metric<number>;
  readonly rpm?: Metric<number>;
}

export interface GpuProcessUtilizationSnapshot {
  readonly overall?: Metric<number>;
  readonly graphics?: Metric<number>;
  readonly compute?: Metric<number>;
  readonly encoder?: Metric<number>;
  readonly decoder?: Metric<number>;
}

export interface GpuProcessSnapshot {
  readonly pid: number;
  readonly name?: string;
  readonly memoryUsedBytes?: Metric<number>;
  readonly utilization?: GpuProcessUtilizationSnapshot;
}

export interface GpuSnapshot {
  readonly sampledAt: number;
  readonly utilization: GpuUtilizationSnapshot;
  readonly memory: GpuMemorySnapshot;
  readonly temperatures: GpuTemperatureSnapshot;
  readonly power: GpuPowerSnapshot;
  readonly clocks: GpuClockSnapshot;
  readonly fan: GpuFanSnapshot;
  readonly processes?: readonly GpuProcessSnapshot[];
}

export interface GpuUtilizationCapabilities {
  readonly overall: boolean;
  readonly graphics: boolean;
  readonly compute: boolean;
  readonly copy: boolean;
  readonly memoryController: boolean;
  readonly encoder: boolean;
  readonly decoder: boolean;
}

export interface GpuCapabilities {
  readonly metrics: readonly GpuMetricName[];
  readonly utilization: GpuUtilizationCapabilities;
  readonly temperature: boolean;
  readonly power: boolean;
  readonly clocks: boolean;
  readonly fan: boolean;
  readonly processes: boolean;
  readonly vendorExtensions: readonly string[];
}

export interface MonitorOpenOptions {
  /** Providers which must initialize successfully. Optional providers never throw. */
  readonly requiredProviders?: readonly string[];
  /** Enable best-effort private Apple telemetry APIs. Defaults to true. */
  readonly enableApplePrivateTelemetry?: boolean;
  /** Include software/render-only adapters in discovery. */
  readonly includeSoftwareAdapters?: boolean;
}

export interface SampleOptions {
  /** Sampling window used for counter-based metrics. */
  readonly windowMs?: number;
  /** Request process-level telemetry when a provider supports it. */
  readonly includeProcesses?: boolean;
}

export interface WatchOptions {
  /** Requested delivery interval. Native sampling may coalesce slower consumers. */
  readonly intervalMs?: number;
  /** Request process-level telemetry when a provider supports it. */
  readonly includeProcesses?: boolean;
  /** Cancels this subscription and completes the iterable without an error. */
  readonly signal?: AbortSignal;
}

export interface ProviderDiagnostics {
  readonly id: string;
  readonly loaded: boolean;
  readonly version?: string;
  readonly devicesMatched?: number;
  readonly reason?: UnavailableReason;
  readonly message?: string;
}

export interface MetricSelectionCandidate {
  readonly source: string;
  readonly score: number;
  readonly selected: boolean;
  readonly quality: MetricQuality;
  readonly sampledAt: number;
}

export interface MetricSelectionDiagnostics {
  readonly deviceId: string;
  readonly metric: GpuMetricName;
  readonly candidates: readonly MetricSelectionCandidate[];
}

export interface GpuDiagnostics {
  readonly platform: string;
  readonly arch: string;
  readonly providers: readonly ProviderDiagnostics[];
  readonly warnings: readonly string[];
  readonly metricSelections?: readonly MetricSelectionDiagnostics[];
}

export interface NvidiaComputeCapability {
  readonly major: number;
  readonly minor: number;
}

export interface NvidiaEccInfo {
  readonly supported: boolean;
  readonly enabled?: boolean;
  readonly correctedErrors?: Metric<number>;
  readonly uncorrectedErrors?: Metric<number>;
}

export interface NvidiaMigPartition {
  readonly id: string;
  readonly uuid?: string;
  readonly gpuInstanceId?: number;
  readonly computeInstanceId?: number;
  readonly memoryTotalBytes?: number;
}

export interface NvidiaMigInfo {
  readonly supported: boolean;
  readonly enabled?: boolean;
  readonly partitions?: readonly NvidiaMigPartition[];
}

export interface NvidiaNvLinkLink {
  readonly index: number;
  readonly active: boolean;
  readonly remotePciAddress?: string;
  readonly transmitBytes?: Metric<number>;
  readonly receiveBytes?: Metric<number>;
}

export interface NvidiaEncoderSession {
  readonly pid?: number;
  readonly codec?: string;
  readonly width?: number;
  readonly height?: number;
  readonly averageFps?: number;
}

export interface NvidiaInfo {
  readonly cudaComputeCapability?: NvidiaComputeCapability;
  readonly smCount?: number;
  readonly pState?: Metric<number>;
  readonly throttleReasons?: Metric<readonly string[]>;
  readonly ecc?: NvidiaEccInfo;
  readonly mig?: NvidiaMigInfo;
  readonly vgpu?: { readonly type?: string; readonly instanceId?: string };
  readonly nvlink?: { readonly links: readonly NvidiaNvLinkLink[] };
  readonly encoderSessions?: readonly NvidiaEncoderSession[];
  readonly decoderSessions?: readonly NvidiaEncoderSession[];
  readonly thermalThresholdsCelsius?: Readonly<Record<string, number>>;
  readonly bar1TotalBytes?: number;
  readonly bar1UsedBytes?: Metric<number>;
  readonly pcieGeneration?: Metric<number>;
  readonly pcieWidth?: Metric<number>;
  readonly computeMode?: string;
}

export interface AmdXgmiLink {
  readonly index: number;
  readonly active: boolean;
  readonly remoteDeviceId?: string;
  readonly bandwidthBytesPerSecond?: Metric<number>;
}

export interface AmdInfo {
  readonly gfxArchitecture?: string;
  readonly computeUnitCount?: number;
  readonly memoryType?: string;
  readonly vramTotalBytes?: number;
  readonly gttTotalBytes?: number;
  readonly hotspotCelsius?: Metric<number>;
  readonly memoryJunctionCelsius?: Metric<number>;
  readonly performanceLevel?: Metric<string>;
  readonly powerProfile?: Metric<string>;
  readonly xgmi?: { readonly links: readonly AmdXgmiLink[] };
  readonly firmware?: Readonly<Record<string, string>>;
}

export interface IntelTileInfo {
  readonly id: string;
  readonly subdeviceId?: number;
  readonly executionUnits?: number;
  readonly xeCores?: number;
}

export interface IntelMemoryRegion {
  readonly type: "device" | "system" | "unknown";
  readonly totalBytes?: number;
  readonly usedBytes?: Metric<number>;
}

export interface IntelEngineGroup {
  readonly name: string;
  readonly utilization?: Metric<number>;
}

export interface IntelInfo {
  readonly xeArchitecture?: string;
  readonly executionUnitCount?: number;
  readonly xeCoreCount?: number;
  readonly xmxAvailable?: boolean;
  readonly tiles?: readonly IntelTileInfo[];
  readonly memoryRegions?: readonly IntelMemoryRegion[];
  readonly engineGroups?: readonly IntelEngineGroup[];
  readonly mediaEngineCount?: number;
}

export type AppleThermalState =
  "nominal" | "fair" | "serious" | "critical" | "unknown";

export interface AppleInfo {
  readonly gpuFamily?: string;
  readonly gpuCoreCount?: number;
  readonly unifiedMemoryTotalBytes?: number;
  readonly activeResidencyPercent?: Metric<number>;
  readonly frequencyScaledActivityPercent?: Metric<number>;
  readonly packageGpuPowerWatts?: Metric<number>;
  readonly thermalState?: Metric<AppleThermalState>;
}

export type GpuErrorCode =
  "monitor-closed" | "invalid-native-data" | "invalid-argument";

export class GpuError extends Error {
  readonly code: GpuErrorCode;

  constructor(code: GpuErrorCode, message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "GpuError";
    this.code = code;
  }
}

export class GpuMonitorClosedError extends GpuError {
  constructor() {
    super("monitor-closed", "The GPU monitor is closed");
    this.name = "GpuMonitorClosedError";
  }
}

export class GpuNativeDataError extends GpuError {
  constructor(path: string, message: string) {
    super(
      "invalid-native-data",
      `Invalid native GPU data at ${path}: ${message}`,
    );
    this.name = "GpuNativeDataError";
  }
}

export class GpuInvalidArgumentError extends GpuError {
  constructor(message: string) {
    super("invalid-argument", message);
    this.name = "GpuInvalidArgumentError";
  }
}
