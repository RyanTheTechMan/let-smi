import type { Gpu } from "../src/index.js";

export function exerciseVendorNarrowing(
  gpu: Gpu,
): Promise<unknown> | undefined {
  if (gpu.vendor === "nvidia") {
    // @ts-expect-error The discriminated union excludes AMD methods here.
    // eslint-disable-next-line @typescript-eslint/no-unsafe-call
    void gpu.amdInfo();
    return gpu.nvidiaInfo();
  }
  if (gpu.vendor === "amd") return gpu.amdInfo();
  if (gpu.vendor === "intel") return gpu.intelInfo();
  if (gpu.vendor === "apple") return gpu.appleInfo();
  return undefined;
}
