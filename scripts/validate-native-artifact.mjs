import { lstat, open } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const targets = new Map([
  [
    "aarch64-apple-darwin",
    { suffix: "darwin-arm64", format: "macho", machine: 0x01_00_00_0c },
  ],
  [
    "x86_64-apple-darwin",
    { suffix: "darwin-x64", format: "macho", machine: 0x01_00_00_07 },
  ],
  [
    "x86_64-pc-windows-msvc",
    { suffix: "win32-x64-msvc", format: "pe", machine: 0x8664 },
  ],
  [
    "aarch64-unknown-linux-gnu",
    { suffix: "linux-arm64-gnu", format: "elf", machine: 183 },
  ],
  [
    "x86_64-unknown-linux-gnu",
    { suffix: "linux-x64-gnu", format: "elf", machine: 62 },
  ],
  [
    "x86_64-unknown-linux-musl",
    { suffix: "linux-x64-musl", format: "elf", machine: 62 },
  ],
]);

const MAX_ARTIFACT_BYTES = 100 * 1024 * 1024;
const MAX_PE_HEADER_OFFSET = 1024 * 1024;

const target = process.argv[2];
if (!target || !targets.has(target)) {
  throw new Error(
    `Usage: node scripts/validate-native-artifact.mjs <target>; received ${target ?? "nothing"}`,
  );
}

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { suffix, format, machine } = targets.get(target);
const fileName = `let-smi.${suffix}.node`;
const candidates = [
  resolve(repositoryRoot, "packages/gpu", fileName),
  resolve(repositoryRoot, "packages/gpu/npm", suffix, fileName),
];
let artifact;
for (const candidate of candidates) {
  try {
    const metadata = await lstat(candidate);
    if (metadata.isFile() && !metadata.isSymbolicLink()) {
      artifact = candidate;
      break;
    }
  } catch {
    // Try the next valid build/assembled-package location.
  }
}
if (artifact === undefined) {
  throw new Error(
    `Native artifact is missing; checked: ${candidates.join(", ")}`,
  );
}
const handle = await open(artifact, "r");
let artifactSize;
try {
  const metadata = await handle.stat();
  if (
    !metadata.isFile() ||
    metadata.size < 1_024 ||
    metadata.size > MAX_ARTIFACT_BYTES
  ) {
    throw new Error(
      `Native artifact is not a regular file or has an implausible size: ${artifact}`,
    );
  }
  artifactSize = metadata.size;

  const readAt = async (length, position) => {
    const buffer = Buffer.alloc(length);
    const { bytesRead } = await handle.read(buffer, 0, length, position);
    if (bytesRead !== length) {
      throw new Error(`Truncated ${format} header in ${artifact}`);
    }
    return buffer;
  };

  if (format === "elf") {
    const header = await readAt(20, 0);
    if (
      !header.subarray(0, 4).equals(Buffer.from([0x7f, 0x45, 0x4c, 0x46])) ||
      header[4] !== 2 ||
      header[5] !== 1 ||
      header.readUInt16LE(18) !== machine
    ) {
      throw new Error(
        `Unexpected ELF class, endianness, or machine in ${artifact}`,
      );
    }
  } else if (format === "pe") {
    const dosHeader = await readAt(64, 0);
    const peOffset = dosHeader.readUInt32LE(0x3c);
    if (
      dosHeader.subarray(0, 2).toString("ascii") !== "MZ" ||
      peOffset < 64 ||
      peOffset > MAX_PE_HEADER_OFFSET ||
      peOffset + 6 > artifactSize
    ) {
      throw new Error(`Unexpected or out-of-bounds PE header in ${artifact}`);
    }
    const peHeader = await readAt(6, peOffset);
    if (
      !peHeader.subarray(0, 4).equals(Buffer.from("PE\0\0", "binary")) ||
      peHeader.readUInt16LE(4) !== machine
    ) {
      throw new Error(`Unexpected PE signature or machine in ${artifact}`);
    }
  } else {
    const header = await readAt(8, 0);
    if (
      header.subarray(0, 4).toString("hex") !== "cffaedfe" ||
      header.readUInt32LE(4) !== machine
    ) {
      throw new Error(`Unexpected 64-bit Mach-O machine in ${artifact}`);
    }
  }
} finally {
  await handle.close();
}

console.log(
  `Validated ${target} artifact (${String(artifactSize)} bytes): ${artifact}`,
);
