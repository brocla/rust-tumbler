// Called by `npm version` lifecycle hook.
// Reads the version npm just wrote to package.json and propagates it to
// src-tauri/tauri.conf.json and src-tauri/Cargo.toml.
const fs = require("fs");

const version = JSON.parse(fs.readFileSync("package.json", "utf8")).version;

// tauri.conf.json
const confPath = "src-tauri/tauri.conf.json";
const conf = JSON.parse(fs.readFileSync(confPath, "utf8"));
conf.version = version;
fs.writeFileSync(confPath, JSON.stringify(conf, null, 2) + "\n");

// Cargo.toml — replace only the top-level `version = "..."` line
// (before any [dependencies] section)
const cargoPath = "src-tauri/Cargo.toml";
let cargo = fs.readFileSync(cargoPath, "utf8");
cargo = cargo.replace(/^version = ".*"/m, `version = "${version}"`);
fs.writeFileSync(cargoPath, cargo);

console.log(`Version synced to ${version}`);
