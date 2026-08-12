"use strict";

const { existsSync } = require("node:fs");
const { join } = require("node:path");

const BINARY_BASENAME = "let-smi";
const MAX_LOADER_ERROR_LENGTH = 1_024;

function safeErrorMessage(error) {
  const message = error instanceof Error ? error.message : String(error);
  const sanitized = message.replace(/[\u0000-\u001f\u007f-\u009f]/gu, " ");
  return sanitized.length <= MAX_LOADER_ERROR_LENGTH
    ? sanitized
    : `${sanitized.slice(0, MAX_LOADER_ERROR_LENGTH)}...`;
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
  if (platform === "win32" && arch === "x64") {
    return ["win32-x64-msvc"];
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

function loadNativeBinding() {
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
