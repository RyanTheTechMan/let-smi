import { appendFile, readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const manifest = JSON.parse(
  await readFile(resolve(repositoryRoot, "packages/gpu/package.json"), "utf8"),
);
const tag = process.argv[2];
const expectedTag = `v${manifest.version}`;
if (tag !== expectedTag) {
  throw new Error(
    `Release tag ${tag ?? "<missing>"} must equal package version tag ${expectedTag}`,
  );
}

const npmTag = manifest.version.includes("-") ? "next" : "latest";
if (process.env.GITHUB_OUTPUT) {
  await appendFile(process.env.GITHUB_OUTPUT, `npm-tag=${npmTag}\n`, "utf8");
}
console.log(`Release ${tag} will publish with npm tag ${npmTag}.`);
