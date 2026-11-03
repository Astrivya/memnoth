#!/usr/bin/env node
"use strict";

// Launcher for the memnoth MCP server.
//
// A release of memnoth-mcp is a single Rust binary per platform, and this package ships one
// copy per platform under `vendor/` when the release script has built it. The launcher exists so
// the same install works in three states of increasing hermeticity: a bundled server binary, an
// explicit `MEMNOTH_BIN`, or whatever `memnoth` is already on PATH from `cargo install` or the
// release installer. A host points at `npx @astrivya/mcp` and gets the server either way, and it
// keeps working when the binary is updated out-of-band.
//
// STDOUT IS THE PROTOCOL. Everything this script says about itself must go to
// stderr; a single stray console.log corrupts the JSON-RPC stream and the host
// reports a parse error instead of a missing install.

const fs = require("node:fs");
const path = require("node:path");
const { spawn } = require("node:child_process");

// Handle --version before doing anything else (works offline after install)
if (process.argv.includes("--version") || process.argv.includes("-v")) {
  const pkgPath = path.join(__dirname, "..", "package.json");
  const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf8"));
  console.log(pkg.version);
  process.exit(0);
}

// Which binary to run, in a fixed order:
//
//   1. a binary bundled in the tarball (`vendor/<platform>-<arch>/memnoth-mcp[.exe]`), which is
//      the standalone server and needs no subcommand;
//   2. `MEMNOTH_BIN`, for a machine that installed memnoth somewhere unusual;
//   3. `memnoth` on PATH, which is what `cargo install` and the release installers produce.
//
// The order is the point: a bundled copy is the most hermetic and predictable. An explicit
// environment variable beats PATH because the person who set it is overriding deliberately.
// Nothing here guesses a path from the user's home directory — a launcher that reads
// `~/.cargo/bin` works on one machine and fails on the next.
function bundledBinary() {
  const name = process.platform === "win32" ? "memnoth-mcp.exe" : "memnoth-mcp";
  const candidate = path.join(
    __dirname,
    "..",
    "vendor",
    `${process.platform}-${process.arch}`,
    name,
  );
  return fs.existsSync(candidate) ? candidate : null;
}

const bundled = bundledBinary();
const env = process.env.MEMNOTH_BIN;
const fromEnv = env && env.trim() ? env.trim() : null;
const binary = bundled || fromEnv || "memnoth";
// The bundled artifact is the standalone `memnoth-mcp` server, which begins
// serving as soon as it starts. A `memnoth` from MEMNOTH_BIN or PATH is the full
// CLI and needs the `mcp serve` subcommand first. Extra argv is passed through in
// both cases so a host can add `--db <path>` from its own server config.
const args = bundled ? process.argv.slice(2) : ["mcp", "serve", ...process.argv.slice(2)];

const child = spawn(binary, args, {
  stdio: "inherit",
  windowsHide: true,
});

child.on("error", (error) => {
  if (error.code === "ENOENT") {
    process.stderr.write(
      `memnoth-mcp: could not find "${binary}".\n` +
        "Looked for a bundled copy in this package, then MEMNOTH_BIN, then PATH.\n" +
        "Install it with `cargo install memnoth`, or download a release from " +
        "https://github.com/amruthjakku/memnoth/releases, then either put it on " +
        "PATH or set MEMNOTH_BIN to its full path.\n"
    );
    process.exit(127);
  }
  process.stderr.write(`memnoth-mcp: failed to start "${binary}": ${error.message}\n`);
  process.exit(1);
});

// Forward termination so a host that stops the server also stops the child,
// rather than leaving an orphan holding the vault database.
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    if (!child.killed) {
      child.kill(signal);
    }
  });
}

child.on("exit", (code, signal) => {
  if (signal) {
    process.exit(1);
  }
  process.exit(code === null ? 1 : code);
});