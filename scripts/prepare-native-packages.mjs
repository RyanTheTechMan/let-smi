import { readdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageRoot = resolve(repositoryRoot, "packages/gpu/npm");
const license = await readFile(resolve(repositoryRoot, "LICENSE"), "utf8");
const entries = await readdir(packageRoot, { withFileTypes: true });
const directories = entries.filter((entry) => entry.isDirectory());

if (directories.length !== 7) {
  throw new Error(
    `Expected seven generated native packages, found ${String(directories.length)}`,
  );
}

for (const directory of directories) {
  const path = join(packageRoot, directory.name);
  const manifestPath = join(path, "package.json");
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  if (
    typeof manifest.name !== "string" ||
    !manifest.name.startsWith("let-smi-")
  ) {
    throw new Error(`Unexpected native package manifest: ${manifestPath}`);
  }
  manifest.publishConfig = {
    ...manifest.publishConfig,
    provenance: true,
  };
  await writeFile(
    manifestPath,
    `${JSON.stringify(manifest, null, 2)}\n`,
    "utf8",
  );
  await writeFile(join(path, "LICENSE"), license, "utf8");
}

console.log(
  `Prepared ${String(directories.length)} native packages with license and provenance metadata.`,
);
