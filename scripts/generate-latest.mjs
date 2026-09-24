import fs from "node:fs";
import path from "node:path";

const [directory, tag, notesFile] = process.argv.slice(2);
if (!directory || !tag) throw new Error("Usage: generate-latest.mjs directory tag");
const version = tag.replace(/^v/, "");
const files = fs.readdirSync(directory);
const mac = files.find((name) => name.endsWith(".app.tar.gz"));
const windows = files.find((name) => name.endsWith("-setup.exe"));
if (!mac || !windows) throw new Error("Universal macOS and Windows updater artifacts are required.");

const platform = (file) => ({
  signature: fs.readFileSync(path.join(directory, `${file}.sig`), "utf8").trim(),
  url: `https://github.com/Von-Van/DelvePlanner/releases/download/${tag}/${file}`,
});
const macEntry = platform(mac);
const windowsEntry = platform(windows);
const notes = notesFile
  ? fs.readFileSync(notesFile, "utf8").trim()
  : `Delve Planner ${tag} public beta. See the GitHub release for complete notes.`;
const metadata = {
  version,
  notes,
  pub_date: new Date().toISOString(),
  platforms: {
    "darwin-aarch64": macEntry,
    "darwin-x86_64": macEntry,
    "windows-x86_64": windowsEntry,
    "windows-x86_64-nsis": windowsEntry,
  },
};
fs.writeFileSync(path.join(directory, "latest.json"), `${JSON.stringify(metadata, null, 2)}\n`);
