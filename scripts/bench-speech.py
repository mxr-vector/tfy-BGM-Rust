#!/usr/bin/env python3
"""Run the headless Rust benchmark and emit a separate process resource report."""

import argparse
import json
import math
from pathlib import Path
import subprocess
import sys
import time

try:
    import psutil
except ImportError:
    psutil = None


def sample_tree(root, cpu_seconds):
    rss = 0
    measured = False
    try:
        children = root.children(recursive=True)
    except psutil.Error:
        children = []
    for process in [root, *children]:
        try:
            with process.oneshot():
                key = (process.pid, process.create_time())
                cpu = process.cpu_times()
                cpu_seconds[key] = max(cpu_seconds.get(key, 0.0), cpu.user + cpu.system)
                rss += process.memory_info().rss
                measured = True
        except psutil.Error:
            continue
    return rss if measured else None


def gpu_sample():
    try:
        output = subprocess.check_output(
            ["nvidia-smi", "--query-gpu=uuid,utilization.gpu,memory.used,memory.free",
             "--format=csv,noheader,nounits"], stderr=subprocess.DEVNULL, text=True, timeout=2,
        )
        rows = (line.split(",") for line in output.splitlines())
        return {uuid.strip(): [int(util), int(used), int(free)] for uuid, util, used, free in rows}
    except (OSError, ValueError, subprocess.SubprocessError):
        return None


def run(command, interval, sample_gpu):
    gpu_baseline = gpu_sample() if sample_gpu else None
    gpu_peak = {}
    last_gpu_sample = 0.0
    cpu_seconds = {}
    peak_rss = None
    samples = 0
    start = time.perf_counter()
    # Pass explicit stdio handles so Windows GUI-subsystem builds can use them.
    with subprocess.Popen(command, stdout=sys.stdout, stderr=sys.stderr) as process:
        root = None
        if psutil is not None:
            try:
                root = psutil.Process(process.pid)
            except psutil.Error:
                pass
        try:
            while True:
                if root is not None:
                    rss = sample_tree(root, cpu_seconds)
                    if rss is not None:
                        samples += 1
                        peak_rss = max(peak_rss or 0, rss)
                if sample_gpu and time.monotonic() - last_gpu_sample >= 0.5:
                    last_gpu_sample = time.monotonic()
                    for uuid, (util, used, _) in (gpu_sample() or {}).items():
                        peak = gpu_peak.setdefault(uuid, {"utilization_pct": 0, "used_mib": 0})
                        peak["utilization_pct"] = max(peak["utilization_pct"], util)
                        peak["used_mib"] = max(peak["used_mib"], used)
                try:
                    process.wait(timeout=interval)
                    break
                except subprocess.TimeoutExpired:
                    pass
        except KeyboardInterrupt:
            process.terminate()
            raise
    return {
        "exit_code": process.returncode,
        "wall_seconds": time.perf_counter() - start,
        "sampler": "psutil" if psutil is not None else None,
        "sample_interval_seconds": interval if psutil is not None else None,
        "sample_count": samples,
        # Sampled tree-wide RSS includes shared pages. Short-lived children and
        # CPU accrued after the last sample can be missed.
        "sampled_peak_tree_rss_bytes": peak_rss,
        "sampled_tree_cpu_seconds": sum(cpu_seconds.values()) if cpu_seconds else None,
        "sampled_gpu_baseline_util_used_free": gpu_baseline,
        "sampled_gpu_peak": gpu_peak or None,
    }


def main():
    parser = argparse.ArgumentParser(
        description="Run EXE --speech-bench and sample its process tree with optional psutil."
    )
    parser.add_argument("--interval", type=float, default=0.1, help="Seconds between samples")
    parser.add_argument("--resources", default="-", help="Resource JSON path (default: stderr)")
    parser.add_argument("--gpu", action="store_true", help="Sample device-wide utilisation and VRAM with nvidia-smi")
    parser.add_argument("exe", help="Path to the tfy-bgm executable")
    parser.add_argument("bench_args", nargs=argparse.REMAINDER, help="Arguments after --speech-bench")
    args = parser.parse_args()
    if not math.isfinite(args.interval) or args.interval <= 0:
        parser.error("--interval must be finite and positive")
    if not args.bench_args:
        parser.error("expected benchmark arguments, e.g. asr MODEL_DIR INPUT.wav")
    try:
        report = run([str(Path(args.exe).resolve()), "--speech-bench", *args.bench_args], args.interval, args.gpu)
        payload = json.dumps(report, indent=2) + "\n"
        if args.resources == "-":
            sys.stderr.write(payload)
        else:
            Path(args.resources).write_text(payload, encoding="utf-8")
    except KeyboardInterrupt:
        return 130
    except (OSError, ValueError) as error:
        print(f"bench-speech: {error}", file=sys.stderr)
        return 1
    code = report["exit_code"]
    return 128 - code if code < 0 else code


if __name__ == "__main__":
    sys.exit(main())
