import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";

const environment = { ...process.env };
for (const name of Object.keys(environment)) {
  if (name.toLowerCase().startsWith("npm_config_")) {
    delete environment[name];
  }
}

const argumentsAfterNpm = process.argv.slice(2);
const command =
  process.platform === "win32"
    ? {
        file: process.execPath,
        arguments: [
          resolve(dirname(process.execPath), "node_modules/npm/bin/npm-cli.js"),
          ...argumentsAfterNpm,
        ],
      }
    : { file: "npm", arguments: argumentsAfterNpm };

const child = spawn(command.file, command.arguments, {
  cwd: process.cwd(),
  env: environment,
  stdio: "inherit",
  windowsHide: true,
});

child.once("error", (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
child.once("exit", (code, signal) => {
  if (signal !== null) {
    console.error(`npm terminated by signal ${signal}`);
    process.exitCode = 1;
  } else {
    process.exitCode = code ?? 1;
  }
});
