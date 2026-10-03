#!/usr/bin/env python3
"""Reproduce independent baseline passes using only the Python standard library."""
import argparse, csv, hashlib, io, json, os, platform, statistics, subprocess, tarfile, time
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
def command(args):
    return subprocess.check_output(args, cwd=ROOT, text=True, stderr=subprocess.STDOUT).strip()
def percentile(values, p):
    values = sorted(values)
    return values[min(len(values)-1, max(0, int((len(values)-1)*p)))]
def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", default="docs/benchmarks/baseline-2026-10-03")
    parser.add_argument("--workers", default="1,2,4,8")
    parser.add_argument("--samples", type=int, default=9)
    args = parser.parse_args()
    if args.samples < 3: raise ValueError("at least 3 repetitions required")
    out = ROOT / args.output
    out.mkdir(parents=True, exist_ok=True)
    workers_list = [int(v) for v in args.workers.split(",")]
    env = os.environ.copy()
    env["WEAVE_BENCH_SAMPLES"] = str(args.samples)
    os.environ["WEAVE_BENCH_SAMPLES"] = str(args.samples)
    paths = []
    for pattern in ["src/**/*.rs", "tests/**/*.rs", "examples/**/*.rs", "benchmarks/src/**/*.rs"]:
        paths.extend(ROOT.glob(pattern))
    paths += [ROOT/p for p in ["Cargo.toml", "Cargo.lock", "benchmarks/Cargo.toml", "benchmarks/Cargo.lock", "benchmarks/run.py"]]
    paths = sorted(set(paths))
    metadata = {
        "started_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "reference_revision": command(["git", "rev-parse", "HEAD"]),
        "remote_roadmap_revision": "1382f4a",
        "git_status": command(["git", "status", "--short"]),
        "rustc": command(["rustc", "-Vv"]), "cargo": command(["cargo", "-V"]),
        "rayon": "1.12.0", "profile": "release opt-level=3 lto=false codegen-units=16 debug=false",
        "platform": platform.platform(), "cpu": command(["lscpu"]),
        "memory": Path("/proc/meminfo").read_text(), "kernel": command(["uname", "-a"]),
        "allowed_cpus": sorted(os.sched_getaffinity(0)), "workers": workers_list,
        "placement": "OS default; no pinning for any engine",
        "numa_sysfs_present": Path("/sys/devices/system/node/online").exists(),
        "warmups_per_scenario_engine": 2, "samples": args.samples,
        "load_average_before": os.getloadavg(), "rustflags": env.get("RUSTFLAGS", ""),
        "source_sha256": {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for p in paths},
        "limitations": [
            "Shared WSL host; CPU frequency and competing Windows load are not controlled.",
            "Warmups and engine order are fixed; no statistical significance or regression thresholds claimed.",
            "Allocation calls/requested bytes are not peak live bytes, RSS or retained memory.",
            "Contention clocks/try_lock/atomics perturb scheduling; diagnostic timings are not throughput baselines.",
            "Rayon has no priority/submit-handle API equivalent: those rows compare checksum-producing work, not identical API semantics.",
            "spawn_empty includes observable accounting and a Weave completion channel; it is not pure empty dispatch.",
            "Cache events, bandwidth, P/E cores and multi-NUMA access require controlled hardware/profiling.",
            "Batch, cancellation and telemetry are not implemented; their section-9 rows are deferred.",
            "No remote GitHub Actions result is claimed.",
        ],
        "commands": [],
    }
    with tarfile.open(out/"source.tar.gz", "w:gz") as archive:
        for path in paths: archive.add(path, arcname=str(path.relative_to(ROOT)))
    (out/"metadata.json").write_text(json.dumps(metadata, indent=2)+"\n")
    logs, summaries = [], []
    modes = {"time": "", "latency": "", "allocations": "allocation-metrics", "contention": "scheduler-metrics"}
    for mode, feature in modes.items():
        cmd = ["cargo", "build", "--locked", "--offline", "--release", "--manifest-path", "benchmarks/Cargo.toml"]
        if feature: cmd += ["--features", feature]
        metadata["commands"].append(cmd)
        target = Path(env.get("CARGO_TARGET_DIR", str(ROOT/"benchmarks/target"))).resolve()
        logs.append(command(cmd))
        for workers in workers_list:
            cmd = [str(target/"release/weave-baseline"), mode, str(workers)]
            metadata["commands"].append(cmd)
            raw = command(cmd)
            name = f"{mode}-{workers}.csv"
            (out/name).write_text(raw+"\n")
            rows = list(csv.DictReader(io.StringIO(raw)))
            groups = {}
            for row in rows: groups.setdefault((row["engine"], row["scenario"]), []).append(row)
            for (engine, scenario), group in groups.items():
                item = {"mode": mode, "engine": engine, "scenario": scenario, "workers": workers, "samples": len(group)}
                if mode == "time":
                    elapsed = [int(r["elapsed_ns"]) for r in group]
                    median = statistics.median(elapsed)
                    item.update(median_ns=median, p95_ns=percentile(elapsed, .95), min_ns=min(elapsed), max_ns=max(elapsed))
                    n = int(group[0]["n"])
                    if n: item["items_per_second"] = n * 1e9 / median
                    assert len({r["checksum"] for r in group}) == 1
                elif mode == "latency":
                    for field in ["dispatch_ns", "completion_ns"]:
                        vals = [int(r[field]) for r in group]
                        item.update({f"{field}_p50": statistics.median(vals), f"{field}_p95": percentile(vals, .95), f"{field}_p99": percentile(vals, .99)})
                elif mode == "allocations":
                    for field in ["allocation_calls", "requested_bytes"]:
                        item[f"{field}_median"] = statistics.median(int(r[field]) for r in group)
                else:
                    for field in ["lock_attempts", "contended", "wait_ns"]:
                        item[f"{field}_median"] = statistics.median(int(r[field]) for r in group)
                summaries.append(item)
            print(f"Recorded {name}: {len(rows)} raw samples", flush=True)
    metadata.update(finished_at_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), load_average_after=os.getloadavg())
    (out/"metadata.json").write_text(json.dumps(metadata, indent=2)+"\n")
    (out/"build.log").write_text("\n".join(logs)+"\n")
    (out/"summary.json").write_text(json.dumps(summaries, indent=2)+"\n")
    lines = [
        "# Mesure initiale Weave / Rayon / séquentiel", "",
        f"Référence : {metadata['reference_revision']}. Le checkout contient des corrections :",
        "ses sources exactes sont conservées dans [source.tar.gz](source.tar.gz), avec leurs empreintes",
        "dans [metadata.json](metadata.json). Dépendances verrouillées, compilateur et matériel enregistrés.", "",
        f"Pools de {workers_list} workers ; 2 échauffements et {args.samples} répétitions par scénario.",
        "Temps, latence, allocations et contention sont exécutés séparément.",
        "Les assertions vérifient le checksum de chaque moteur sur les mêmes entrées.",
        "L'équivalent séquentiel n'utilise pas de pool.", "",
        "## Comparaisons de temps en régime établi", "",
        "Médianes en ms ; dispersion et débits détaillés dans [summary.json](summary.json).",
    ]
    for workers in workers_list:
        lines += ["", f"### {workers} worker(s)", ""]
        for scenario in ["submit_small", "external_producers", "join", "map_light", "map_heavy", "filter_reduce", "collect", "memory_scattered"]:
            values = {s["engine"]: s["median_ns"] for s in summaries if s["mode"]=="time" and s["workers"]==workers and s["scenario"]==scenario}
            lines.append(f"- {scenario} : Weave {values['weave']/1e6:.3f} ms ; Rayon {values['rayon']/1e6:.3f} ms ; séquentiel {values['sequential']/1e6:.3f} ms.")
    lines += [
        "", "## Données et reproduction", "",
        "Les fichiers time-N.csv, latency-N.csv, allocations-N.csv et contention-N.csv conservent les mesures brutes.",
        "La création/destruction du pool est une ligne séparée de time-N.csv.",
        "Les latences sont appel de soumission→début et appel de soumission→fin du calcul pour une rafale de 256 jobs,",
        "hors restitution du handle et notification finale. Le verrou de collecte est hors intervalle mesuré.",
        "Les allocations couvrent le passage et ses valeurs temporaires, tous threads confondus.",
        "La contention mesure tentatives, try_lock occupés et temps cumulé d'attente du mutex central de Weave.",
        "Rayon ne fournit pas un compteur comparable ; aucun chiffre de contention Rayon n'est inventé.", "",
        "~~~text",
        "CARGO_TARGET_DIR=/tmp/weave-bench-phase0 python3 benchmarks/run.py --workers 1,2,4,8 --samples 9 --output docs/benchmarks/nouvelle-mesure",
        "~~~", "",
        "Pour reproduire cette source précise, extraire source.tar.gz dans un dossier vide, installer le compilateur",
        "enregistré et utiliser les Cargo.lock conservés. Une autre machine peut donner d'autres temps.",
        "", "## Limites", "",
    ] + ["- "+v for v in metadata["limitations"]]
    (out/"REPORT.md").write_text("\n".join(lines)+"\n")
if __name__ == "__main__": main()
