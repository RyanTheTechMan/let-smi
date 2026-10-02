import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const child = spawn(
  process.execPath,
  [resolve(root, "scripts/test-intel-hardware.mjs"), ...process.argv.slice(2)],
  { cwd: root, stdio: ["ignore", "pipe", "pipe"], windowsHide: true },
);
let stdout = "";
let stderr = "";
child.stdout.setEncoding("utf8");
child.stderr.setEncoding("utf8");
child.stdout.on("data", (value) => {
  stdout += value;
});
child.stderr.on("data", (value) => {
  stderr += value;
});
const deadline = setTimeout(() => {
  child.kill();
}, 40_000);
const code = await new Promise((resolvePromise, reject) => {
  child.once("error", reject);
  child.once("close", resolvePromise);
}).finally(() => clearTimeout(deadline));
if (code !== 0)
  throw new Error(
    `Intel hardware validation failed or exceeded 40 seconds: ${stderr}`,
  );
JSON.parse(stdout);
process.stdout.write(stdout);
