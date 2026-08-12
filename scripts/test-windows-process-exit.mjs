import { strict as assert } from "node:assert";
import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const childScript = resolve(
  repositoryRoot,
  "scripts/test-windows-hardware.mjs",
);
const deadlineMs = 20_000;

const result = await new Promise((resolvePromise, reject) => {
  const child = spawn(process.execPath, [childScript], {
    cwd: repositoryRoot,
    env: process.env,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => {
    stdout += chunk;
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  const timer = setTimeout(() => {
    child.kill();
    reject(
      new Error(
        `Windows hardware child did not exit within ${String(deadlineMs)} ms`,
      ),
    );
  }, deadlineMs);
  child.once("error", (error) => {
    clearTimeout(timer);
    reject(error);
  });
  child.once("exit", (code, signal) => {
    clearTimeout(timer);
    resolvePromise({ code, signal, stdout, stderr });
  });
});

assert.equal(
  result.code,
  0,
  `hardware child failed (${result.signal ?? "no signal"}): ${result.stderr}`,
);
const report = JSON.parse(result.stdout);
if (report.skipped) {
  console.log(`Windows hardware integration skipped: ${report.reason}`);
} else {
  assert(report.fsReadWhileFourStreamsPendingMs < 750);
  assert(report.closeMs < 3_000);
  console.log(
    `Windows hardware child exited within ${String(deadlineMs)} ms; fs read ${String(report.fsReadWhileFourStreamsPendingMs)} ms, close ${String(report.closeMs)} ms.`,
  );
}
