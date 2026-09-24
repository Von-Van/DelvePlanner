import fs from "node:fs";

const packageJson = JSON.parse(fs.readFileSync("package.json", "utf8"));
const tauriConfig = JSON.parse(fs.readFileSync("src-tauri/tauri.conf.json", "utf8"));
const cargo = fs.readFileSync("src-tauri/Cargo.toml", "utf8");
const cargoVersion = cargo.match(/^version = "([^"]+)"/m)?.[1];
const versions = [packageJson.version, tauriConfig.version, cargoVersion];

if (versions.some((version) => !version) || new Set(versions).size !== 1) {
  console.error(`Version mismatch: package=${versions[0]} tauri=${versions[1]} cargo=${versions[2]}`);
  process.exit(1);
}

console.log(`Delve Planner versions synchronized at ${versions[0]}.`);
