// Called by `npm version` lifecycle hook.
// Reads the version npm just wrote to package.json and propagates it to
// src-tauri/tauri.conf.json and src-tauri/Cargo.toml.
import { readFileSync, writeFileSync } from "fs";

const version = JSON.parse(readFileSync("package.json", "utf8")).version;

// tauri.conf.json
const confPath = "src-tauri/tauri.conf.json";
const conf = JSON.parse(readFileSync(confPath, "utf8"));
conf.version = version;
writeFileSync(confPath, JSON.stringify(conf, null, 2) + "\n");

// Cargo.toml — replace only the top-level `version = "..."` line
const cargoPath = "src-tauri/Cargo.toml";
let cargo = readFileSync(cargoPath, "utf8");
cargo = cargo.replace(/^version = ".*"/m, `version = "${version}"`);
writeFileSync(cargoPath, cargo);

// Cargo.lock — update only this crate's [[package]] entry. Scoped to the line
// immediately after `name = "tumbler"` so it never touches dependency versions.
const lockPath = "src-tauri/Cargo.lock";
let lock = readFileSync(lockPath, "utf8");
lock = lock.replace(
  /(name = "tumbler"\r?\nversion = ")[^"]*"/,
  `$1${version}"`,
);
writeFileSync(lockPath, lock);

console.log(`Version synced to ${version}`);
