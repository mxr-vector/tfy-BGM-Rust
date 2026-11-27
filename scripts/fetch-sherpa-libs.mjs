#!/usr/bin/env node
/**
 * Prefetch the prebuilt sherpa-onnx static libraries and print the lib dir.
 *
 * Why this exists
 * ---------------
 * `sherpa-onnx-sys` downloads a ~127 MB archive from GitHub Releases inside its
 * build script, with no retry and no mirror, and it short-circuits as soon as
 * `target/sherpa-onnx-prebuilt/<stem>/lib` exists (contents are never checked).
 * On CI that combination fails in two ways:
 *
 *   1. A flaky / blocked download aborts the build script midway.
 *   2. A cache restore can bring back the build-script fingerprint (which
 *      records `-L .../sherpa-onnx-prebuilt/.../lib`) while the extracted libs
 *      are missing, so rustc fails with
 *      "could not find native static library `sherpa-onnx-c-api`".
 *
 * This script removes both: it fetches the archive once (with retries and a
 * size check), extracts it into a stable directory outside `target/`, verifies
 * that all required libraries are present, and prints the path so the packaging
 * scripts can export `SHERPA_ONNX_LIB_DIR`. The build script then never touches
 * the network and `-L` always points at a verified directory.
 *
 * Usage:
 *   node scripts/fetch-sherpa-libs.mjs [options]
 *
 *   --target-os <windows|linux>   default: host OS
 *   --target-arch <x64|arm64>     default: host arch (x86_64/aarch64 also accepted)
 *   --cache-dir <dir>             default: <repo>/.cache/sherpa-onnx
 *   --force                       re-extract even if the lib dir already looks good
 *   --dry-run                     print the resolved archive URL and exit
 *
 * stdout: the absolute path of the verified lib dir (single line).
 * stderr: progress and diagnostics.
 *
 * Requires curl and tar on PATH (both ship with Windows 10+ and every runner).
 */

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.dirname(SCRIPT_DIR);
const CARGO_LOCK = path.join(REPO_ROOT, "src-tauri", "Cargo.lock");

const GITHUB_RELEASE_BASE =
  "https://github.com/k2-fsa/sherpa-onnx/releases/download";

// Static libs that sherpa-onnx-sys emits as `-l static=<name>`; missing any of
// them produces the "could not find native static library" link error.
const STATIC_LIBS = [
  "sherpa-onnx-c-api",
  "sherpa-onnx-core",
  "kaldi-hmm-gmm-core",
  "kaldi-native-io-core",
  "kaldi-decoder-core",
  "sherpa-onnx-kaldifst-core",
  "sherpa-onnx-fstfar",
  "sherpa-onnx-fst",
  "kaldi-native-fbank-core",
  "kissfft-float",
  "onnxruntime",
  "ssentencepiece_core",
  "utf8proc",
];

const MIN_TOTAL_BYTES = 20 * 1024 * 1024;

const log = (message) => process.stderr.write(`${message}\n`);

function die(message) {
  process.stderr.write(`error: ${message}\n`);
  process.exit(1);
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    stdio: options.capture ? ["ignore", "pipe", "pipe"] : "inherit",
    encoding: "utf8",
    env: process.env,
    cwd: options.cwd,
  });
  if (result.error) {
    die(`cannot run ${command}: ${result.error.message}`);
  }
  return result;
}

function parseArgs(argv) {
  const options = {
    targetOs: "",
    targetArch: "",
    cacheDir: "",
    force: false,
    dryRun: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      const value = argv[i + 1];
      if (value === undefined) die(`${arg} requires a value`);
      i += 1;
      return value;
    };
    switch (arg) {
      case "--target-os":
        options.targetOs = next();
        break;
      case "--target-arch":
        options.targetArch = next();
        break;
      case "--cache-dir":
        options.cacheDir = next();
        break;
      case "--force":
        options.force = true;
        break;
      case "--dry-run":
        options.dryRun = true;
        break;
      case "-h":
      case "--help":
        process.stdout.write(
          "usage: node scripts/fetch-sherpa-libs.mjs [--target-os windows|linux] [--target-arch x64|arm64] [--cache-dir DIR] [--force] [--dry-run]\n",
        );
        process.exit(0);
        break;
      default:
        die(`unknown argument: ${arg}`);
    }
  }

  return options;
}

function normalizeOs(value) {
  const osName = (value || os.platform()).toLowerCase();
  if (osName === "win32" || osName === "windows") return "windows";
  if (osName === "linux") return "linux";
  die(`unsupported target OS: ${osName}`);
}

function normalizeArch(value) {
  const arch = (value || os.arch()).toLowerCase();
  if (arch === "x64" || arch === "x86_64" || arch === "amd64") return "x64";
  if (arch === "arm64" || arch === "aarch64") return "arm64";
  die(`unsupported target arch: ${arch}`);
}

// The version must follow Cargo.lock, otherwise a dependency bump would silently
// keep using an older native build.
function readSherpaVersion() {
  if (!fs.existsSync(CARGO_LOCK)) die(`Cargo.lock not found: ${CARGO_LOCK}`);
  const blocks = fs.readFileSync(CARGO_LOCK, "utf8").split("[[package]]");
  for (const block of blocks) {
    if (!/^\s*name = "sherpa-onnx-sys"$/m.test(block)) continue;
    const version = block.match(/^\s*version = "([^"]+)"/m);
    if (version) return version[1];
  }
  die("sherpa-onnx-sys not found in src-tauri/Cargo.lock");
}

function archiveName(version, targetOs, targetArch) {
  const v = `sherpa-onnx-v${version}`;
  switch (`${targetOs}/${targetArch}`) {
    case "windows/x64":
      return `${v}-win-x64-static-MT-Release-lib.tar.bz2`;
    case "windows/arm64":
      return `${v}-win-arm64-static-MT-Release-lib.tar.bz2`;
    case "linux/x64":
      return `${v}-linux-x64-static-lib.tar.bz2`;
    case "linux/arm64":
      return `${v}-linux-aarch64-static-lib.tar.bz2`;
    default:
      die(`no sherpa-onnx prebuilt archive for ${targetOs}/${targetArch}`);
  }
}

function libFileNames(targetOs) {
  return STATIC_LIBS.map((name) =>
    targetOs === "windows" ? `${name}.lib` : `lib${name}.a`,
  );
}

/**
 * Returns true when every required library exists and the overall size looks
 * sane. A half-extracted directory is exactly what makes rustc fail later, so
 * the check has to look at contents rather than just the directory.
 */
function libDirIsUsable(dir, targetOs, quiet = false) {
  if (!dir || !fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) return false;

  let total = 0;
  const missing = [];
  for (const file of libFileNames(targetOs)) {
    const full = path.join(dir, file);
    if (!fs.existsSync(full)) {
      missing.push(file);
      continue;
    }
    total += fs.statSync(full).size;
  }

  if (missing.length > 0) {
    if (!quiet) log(`  incomplete lib dir (${missing.length} missing): ${dir}`);
    return false;
  }
  if (total < MIN_TOTAL_BYTES) {
    if (!quiet) {
      log(
        `  lib dir is suspiciously small (${total} bytes), treating as broken: ${dir}`,
      );
    }
    return false;
  }
  return true;
}

function cargoTargetDir() {
  const explicit = process.env.CARGO_TARGET_DIR;
  if (explicit) return path.resolve(REPO_ROOT, explicit);
  return path.join(REPO_ROOT, "src-tauri", "target");
}

function toTarPath(target) {
  // Relative paths only: GNU tar parses `-f D:/x/y.tar.bz2` as the remote host
  // "D" ("Cannot connect to D: resolve failed"), so absolute paths are never
  // handed to it.
  return target.replace(/\\/g, "/");
}

function tarCandidates() {
  if (process.platform !== "win32") {
    return [{ command: "tar", absolute: false }];
  }
  // Git Bash puts GNU tar first on PATH; Windows also ships bsdtar. The GNU
  // build only accepts relative paths (see toTarPath), bsdtar accepts absolute
  // ones, so a failed attempt is retried with the bundled bsdtar.
  const systemRoot = process.env.SystemRoot || "C:\\Windows";
  return [
    { command: "tar", absolute: false },
    { command: path.join(systemRoot, "System32", "tar.exe"), absolute: true },
  ];
}

function rmrf(target) {
  fs.rmSync(target, { recursive: true, force: true });
}

function extract(archivePath, workDir, stem, targetOs) {
  const staging = path.join(workDir, `extract-${process.pid}`);
  let lastError = "";

  for (const candidate of tarCandidates()) {
    if (candidate.absolute && !fs.existsSync(candidate.command)) continue;

    rmrf(staging);
    fs.mkdirSync(staging, { recursive: true });

    const args = candidate.absolute
      ? ["-xjf", toTarPath(archivePath), "-C", toTarPath(staging)]
      : [
          "-xjf",
          toTarPath(path.relative(workDir, archivePath)),
          "-C",
          toTarPath(path.basename(staging)),
        ];

    log(`  extracting with ${candidate.command}`);
    const result = run(candidate.command, args, {
      cwd: candidate.absolute ? undefined : workDir,
    });
    if (result.status !== 0) {
      lastError = `${candidate.command} exited ${result.status}`;
      continue;
    }

    const stagedLib = path.join(staging, stem, "lib");
    if (!libDirIsUsable(stagedLib, targetOs)) {
      lastError = `${path.basename(archivePath)} has no complete lib/ directory`;
      continue;
    }

    const finalDir = path.join(workDir, stem);
    rmrf(finalDir);
    fs.renameSync(path.join(staging, stem), finalDir);
    rmrf(staging);
    return path.join(finalDir, "lib");
  }

  rmrf(staging);
  die(`could not extract ${path.basename(archivePath)} (${lastError})`);
}

function download(url, destination) {
  const partPath = `${destination}.part`;
  fs.mkdirSync(path.dirname(destination), { recursive: true });
  rmrf(partPath);

  log(`  fetching ${url}`);
  const result = run(
    "curl",
    [
      "--fail",
      "--location",
      "--silent",
      "--show-error",
      "--retry",
      "4",
      "--retry-delay",
      "3",
      "--connect-timeout",
      "20",
      "--speed-limit",
      "1024",
      "--speed-time",
      "60",
      "--output",
      partPath,
      url,
    ],
    { capture: true },
  );

  if (result.status === 0) {
    const size = fs.statSync(partPath).size;
    if (size >= MIN_TOTAL_BYTES) {
      fs.renameSync(partPath, destination);
      log(`  downloaded ${(size / 1024 / 1024).toFixed(1)} MB`);
      return;
    }
    rmrf(partPath);
    die(`${url} returned only ${size} bytes`);
  }

  const detail = (result.stderr || "").trim().split("\n").pop();
  rmrf(partPath);
  die(
    [
      `could not download ${path.basename(destination)} (curl exit ${result.status}): ${detail}`,
      "  Copy the archive manually into .cache/sherpa-onnx/archives/ and re-run,",
      "  or set SHERPA_ONNX_LIB_DIR to a directory that already holds the libraries.",
    ].join("\n"),
  );
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  const targetOs = normalizeOs(options.targetOs);
  const targetArch = normalizeArch(options.targetArch);
  const version = readSherpaVersion();
  const archive = archiveName(version, targetOs, targetArch);
  const stem = archive.replace(/\.tar\.bz2$/, "");

  const cacheDir = path.resolve(
    REPO_ROOT,
    options.cacheDir || path.join(".cache", "sherpa-onnx"),
  );
  const githubUrl = `${GITHUB_RELEASE_BASE}/v${version}/${archive}`;

  if (options.dryRun) {
    process.stdout.write(
      [
        `os=${targetOs} arch=${targetArch} sherpa-onnx-sys=${version}`,
        `archive=${archive}`,
        `url=${githubUrl}`,
        `cache=${cacheDir}`,
      ].join("\n") + "\n",
    );
    return;
  }

  log(`sherpa-onnx ${version} ${targetOs}/${targetArch}`);

  // 1. Honour an explicit SHERPA_ONNX_LIB_DIR instead of fighting it.
  const fromEnv = process.env.SHERPA_ONNX_LIB_DIR;
  if (fromEnv && libDirIsUsable(path.resolve(fromEnv), targetOs)) {
    log(`  reusing SHERPA_ONNX_LIB_DIR: ${path.resolve(fromEnv)}`);
    process.stdout.write(`${path.resolve(fromEnv)}\n`);
    return;
  }

  // 2. Reuse whatever a previous build already extracted into target/, so an
  //    offline machine keeps working without re-downloading.
  const targetLib = path.join(
    cargoTargetDir(),
    "sherpa-onnx-prebuilt",
    stem,
    "lib",
  );
  if (!options.force && libDirIsUsable(targetLib, targetOs)) {
    log(`  reusing build cache: ${targetLib}`);
    process.stdout.write(`${targetLib}\n`);
    return;
  }

  // 3. Reuse the verified copy kept outside target/.
  const cachedLib = path.join(cacheDir, stem, "lib");
  if (!options.force && libDirIsUsable(cachedLib, targetOs)) {
    log(`  reusing prefetched copy: ${cachedLib}`);
    process.stdout.write(`${cachedLib}\n`);
    return;
  }

  // 4. Download and extract.
  const archivePath = path.join(cacheDir, "archives", archive);
  const haveArchive =
    fs.existsSync(archivePath) &&
    fs.statSync(archivePath).size >= MIN_TOTAL_BYTES;
  if (haveArchive) {
    log(`  using cached archive: ${archivePath}`);
  } else {
    log(`  archive not cached, downloading`);
    download(githubUrl, archivePath);
  }

  const libDir = extract(archivePath, cacheDir, stem, targetOs);
  const total = fs
    .readdirSync(libDir)
    .reduce((sum, file) => sum + fs.statSync(path.join(libDir, file)).size, 0);
  log(
    `  verified ${libFileNames(targetOs).length} libraries, ${(total / 1024 / 1024).toFixed(0)} MB`,
  );

  process.stdout.write(`${libDir}\n`);
}

main();
