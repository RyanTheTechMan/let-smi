import { readdir, readFile } from "node:fs/promises";
import { dirname, extname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const roots = [
  resolve(repositoryRoot, "crates"),
  resolve(repositoryRoot, "packages/gpu/src"),
];
const explicitFiles = [resolve(repositoryRoot, "packages/gpu/native.cjs")];
const sourceExtensions = new Set([".rs", ".ts", ".js", ".cjs", ".mjs"]);
const prohibited = [
  ["Node child process API", /(?:node:)?child_process/u],
  [
    "Rust process Command",
    /(?:std::process::Command|\bCommand\s*::\s*(?:new|spawn))/u,
  ],
  ["C process launcher", /\b(?:popen|posix_spawn|execvp|execlp)\s*\(/u],
  [
    "shell command mode",
    /(?:["'](?:sh|bash|zsh|cmd|powershell)["']).{0,80}["'](?:-c|\/c|-[Cc]ommand)["']/su,
  ],
  ["nvidia-smi executable", /\bnvidia-smi\b/u],
  ["amd-smi executable", /\bamd-smi\b/u],
  ["intel_gpu_top executable", /\bintel_gpu_top\b/u],
  ["powermetrics executable", /\bpowermetrics\b/u],
];

const files = [...explicitFiles];
for (const root of roots) await collectSourceFiles(root, files);

const violations = [];
for (const file of files) {
  const source = await readFile(file, "utf8");
  for (const [description, pattern] of prohibited) {
    if (pattern.test(source)) {
      violations.push(`${relative(repositoryRoot, file)}: ${description}`);
    }
  }
}

if (violations.length > 0) {
  throw new Error(
    `Runtime subprocess policy violations:\n${violations.join("\n")}`,
  );
}
console.log(
  `No subprocess APIs or telemetry executables found in ${files.length} runtime files.`,
);

async function collectSourceFiles(directory, output) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      await collectSourceFiles(path, output);
    } else if (entry.isFile() && sourceExtensions.has(extname(entry.name))) {
      output.push(path);
    }
  }
}
