"use strict";

const { existsSync } = require("node:fs");
const { isAbsolute, join, resolve } = require("node:path");

const BINARY_BASENAME = "let-smi";

function safeErrorMessage(error) {
  if (error instanceof Error) return error.message.replaceAll("\n", " ");
  return String(error).replaceAll("\n", " ");
}

function linuxLibc() {
  try {
    const report = process.report?.getReport?.();
    if (report?.header?.glibcVersionRuntime) return "gnu";
    const sharedObjects = Array.isArray(report?.sharedObjects)
      ? report.sharedObjects
      : [];
    if (
      sharedObjects.some(
        (entry) =>
          typeof entry === "string" &&
          (entry.includes("ld-musl-") || entry.includes("libc.musl-")),
      )
    ) {
      return "musl";
    }
  } catch {
    // An embedded Node runtime may disable reports. Try compatible targets in
    // a deterministic order below; never launch an executable to detect libc.
  }
  return "unknown";
}

function candidateTargets() {
  const { platform, arch } = process;
  if (platform === "darwin" && (arch === "arm64" || arch === "x64")) {
    return [`darwin-${arch}`];
  }
  if (platform === "win32" && (arch === "arm64" || arch === "x64")) {
    return [`win32-${arch}-msvc`];
  }
  if (platform === "linux" && arch === "arm64") {
    return ["linux-arm64-gnu"];
  }
  if (platform === "linux" && arch === "x64") {
    const libc = linuxLibc();
    if (libc === "gnu") return ["linux-x64-gnu"];
    if (libc === "musl") return ["linux-x64-musl"];
    return ["linux-x64-gnu", "linux-x64-musl"];
  }
  return [];
}

function normalizeBinding(value, source) {
  const candidate =
    value && typeof value.openMonitor === "function" ? value : value?.default;
  if (!candidate || typeof candidate.openMonitor !== "function") {
    throw new TypeError(`${source} does not export openMonitor()`);
  }
  return candidate;
}

function loadOverride() {
  const configuredPath = process.env.NAPI_RS_NATIVE_LIBRARY_PATH;
  if (!configuredPath) return undefined;
  const resolvedPath = isAbsolute(configuredPath)
    ? configuredPath
    : resolve(process.cwd(), configuredPath);
  if (!existsSync(resolvedPath)) {
    throw new Error(
      `NAPI_RS_NATIVE_LIBRARY_PATH does not identify an existing file: ${resolvedPath}`,
    );
  }
  return normalizeBinding(require(resolvedPath), resolvedPath);
}

function loadNativeBinding() {
  const override = loadOverride();
  if (override !== undefined) return override;

  const targets = candidateTargets();
  if (targets.length === 0) {
    throw new Error(
      `let-smi has no prebuilt native addon for ${process.platform}/${process.arch}`,
    );
  }

  const failures = [];
  for (const target of targets) {
    const localPath = join(__dirname, `${BINARY_BASENAME}.${target}.node`);
    if (existsSync(localPath)) {
      try {
        return normalizeBinding(require(localPath), localPath);
      } catch (error) {
        failures.push(`${localPath}: ${safeErrorMessage(error)}`);
      }
    }

    const packageName = `${BINARY_BASENAME}-${target}`;
    try {
      return normalizeBinding(require(packageName), packageName);
    } catch (error) {
      failures.push(`${packageName}: ${safeErrorMessage(error)}`);
    }
  }

  throw new Error(
    [
      `Unable to load the let-smi native addon for ${process.platform}/${process.arch}.`,
      "The matching platform package may be absent, or the addon could not be loaded.",
      "Reinstall with optional dependencies enabled and use a supported Node.js version.",
      ...failures.map((failure) => `- ${failure}`),
    ].join("\n"),
  );
}

module.exports = loadNativeBinding();
