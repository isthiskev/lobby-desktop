// Release build: signs the updater artifacts and stages everything a GitHub
// release needs in dist-release/.
//
// Running apps only accept installers signed with the private key at
// ~/.tauri/lobby-desktop.key (its public half is baked into tauri.conf.json),
// so a plain `tauri build` produces installers existing installs would refuse
// to auto-update to. Always release through this script.
import { execSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const keyPath = path.join(homedir(), ".tauri", "lobby-desktop.key");
if (!existsSync(keyPath)) {
  console.error(
    `Updater signing key not found at ${keyPath}.\n` +
      "Without it this build cannot be signed and shipped apps will refuse to\n" +
      "auto-update to it. Restore the key from backup before releasing.",
  );
  process.exit(1);
}

const conf = JSON.parse(
  readFileSync(path.join(root, "src-tauri", "tauri.conf.json"), "utf8"),
);
const version = conf.version;

execSync("npx tauri build", {
  cwd: root,
  stdio: "inherit",
  env: {
    ...process.env,
    TAURI_SIGNING_PRIVATE_KEY: keyPath,
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "",
  },
});

const bundle = path.join(root, "src-tauri", "target", "release", "bundle");
const setupExe = path.join(bundle, "nsis", `Lobby_${version}_x64-setup.exe`);
const setupSig = `${setupExe}.sig`;
const msi = path.join(bundle, "msi", `Lobby_${version}_x64_en-US.msi`);

const out = path.join(root, "dist-release");
rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

// Stable asset names: humans always grab "Lobby-setup.exe", and latest.json
// points at the same file it ships beside.
copyFileSync(setupExe, path.join(out, "Lobby-setup.exe"));
if (existsSync(msi)) copyFileSync(msi, path.join(out, "Lobby.msi"));

const manifest = {
  version,
  pub_date: new Date().toISOString(),
  platforms: {
    "windows-x86_64": {
      signature: readFileSync(setupSig, "utf8").trim(),
      url: `https://github.com/isthiskev/lobby-desktop/releases/download/v${version}/Lobby-setup.exe`,
    },
  },
};
writeFileSync(
  path.join(out, "latest.json"),
  JSON.stringify(manifest, null, 2) + "\n",
);

console.log(
  `\nStaged in dist-release/ — create GitHub release v${version} and upload ALL of:`,
);
console.log("  Lobby-setup.exe   installer; also what the auto-updater downloads");
if (existsSync(msi)) console.log("  Lobby.msi         optional MSI package");
console.log(
  "  latest.json       update manifest — REQUIRED, running apps poll it via releases/latest",
);
