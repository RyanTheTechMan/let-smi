import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
      "let-smi-win32-arm64-msvc",
      "let-smi-win32-x64-msvc",
    ]);
  });

  it("loads an explicit native override without eager platform probing", async () => {
    const loaderPath = fileURLToPath(new URL("../native.cjs", import.meta.url));
    const fixtureDirectory = await mkdtemp(join(tmpdir(), "let-smi-loader-"));
    const fakeNativePath = join(fixtureDirectory, "fake-native.cjs");
    await writeFile(
      fakeNativePath,
      "module.exports = { openMonitor() { return {}; } };\n",
      "utf8",
    );
    const localRequire = createRequire(import.meta.url);
    const previousOverride = process.env.NAPI_RS_NATIVE_LIBRARY_PATH;
    try {
      process.env.NAPI_RS_NATIVE_LIBRARY_PATH = fakeNativePath;
      Reflect.deleteProperty(localRequire.cache, loaderPath);
      const binding = localRequire(loaderPath) as unknown;
      expect(
        typeof (binding as { readonly openMonitor?: unknown }).openMonitor,
      ).toBe("function");
    } finally {
      Reflect.deleteProperty(localRequire.cache, loaderPath);
      if (previousOverride === undefined) {
        delete process.env.NAPI_RS_NATIVE_LIBRARY_PATH;
      } else {
        process.env.NAPI_RS_NATIVE_LIBRARY_PATH = previousOverride;
      }
      await rm(fixtureDirectory, { recursive: true, force: true });
    }
  });
});
