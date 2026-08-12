import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Worker } from "node:worker_threads";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

describe("native loader", () => {
  it("does not import subprocess APIs or execute external programs", async () => {
    const loaderPath = fileURLToPath(new URL("../native.cjs", import.meta.url));
    const source = await readFile(loaderPath, "utf8");
    expect(source).not.toMatch(/node:child_process|child_process/);
    expect(source).not.toMatch(/\bexec(?:File|Sync)?\s*\(/);
    expect(source).not.toMatch(/\bspawn(?:Sync)?\s*\(/);
  });

  it("uses process reports for libc detection", async () => {
    const loaderPath = fileURLToPath(new URL("../native.cjs", import.meta.url));
    const source = await readFile(loaderPath, "utf8");
    expect(source).toContain("process.report");
    expect(source).toContain("glibcVersionRuntime");
  });

  it("contains every published platform target", async () => {
    const manifestPath = fileURLToPath(
      new URL("../package.json", import.meta.url),
    );
    const manifest = JSON.parse(
      await readFile(manifestPath, "utf8"),
    ) as unknown;
    if (typeof manifest !== "object" || manifest === null) {
      throw new Error("package manifest is not an object");
    }
    const optionalDependencies = (
      manifest as { readonly optionalDependencies?: unknown }
    ).optionalDependencies;
    if (
      typeof optionalDependencies !== "object" ||
      optionalDependencies === null
    ) {
      throw new Error("optionalDependencies is not an object");
    }
    expect(Object.keys(optionalDependencies).sort()).toEqual([
      "let-smi-darwin-arm64",
      "let-smi-darwin-x64",
      "let-smi-linux-arm64-gnu",
      "let-smi-linux-x64-gnu",
      "let-smi-linux-x64-musl",
      "let-smi-win32-x64-msvc",
    ]);
  });

  it("ignores generic native overrides and reports an actionable missing-package error", async () => {
    const loaderPath = fileURLToPath(new URL("../native.cjs", import.meta.url));
    const fixtureDirectory = await mkdtemp(join(tmpdir(), "let-smi-missing-"));
    const isolatedLoader = join(fixtureDirectory, "native.cjs");
    const fakeNativePath = join(fixtureDirectory, "fake-native.cjs");
    await writeFile(isolatedLoader, await readFile(loaderPath, "utf8"), "utf8");
    await writeFile(
      fakeNativePath,
      "throw new Error('generic native override was executed');\n",
      "utf8",
    );
    try {
      const message = await new Promise<string>((resolvePromise, reject) => {
        const worker = new Worker(
          `
            const { parentPort, workerData } = require("node:worker_threads");
            process.env.NAPI_RS_NATIVE_LIBRARY_PATH = workerData.fakeNativePath;
            try {
              require(workerData.loaderPath);
              parentPort.postMessage("");
            } catch (error) {
              parentPort.postMessage(error instanceof Error ? error.message : String(error));
            }
          `,
          {
            eval: true,
            workerData: { loaderPath: isolatedLoader, fakeNativePath },
          },
        );
        worker.once("message", resolvePromise);
        worker.once("error", reject);
      });
      expect(message).toContain("Unable to load the let-smi native addon");
      expect(message).toContain("Reinstall with optional dependencies enabled");
      expect(message).not.toContain("generic native override was executed");
    } finally {
      await rm(fixtureDirectory, { recursive: true, force: true });
    }
  });
});
