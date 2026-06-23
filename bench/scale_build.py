"""Stage 1: build fresh N-stores + parity exports + title->iri maps for the scale sweep (rust-rfcs).

Each store is rebuilt from the pinned RFC text (parse_rfc) so it is self-contained; the nested
distractor ladder uses the SAME fixed-seed shuffle as scale_probe, so B1 (BM25 over the export) and B2
(graph) see identical content at each N. record_important_decision mints a fresh UUID per call, so the
per-store target IRI is resolved from that store's parity export and written to a title->iri map.

  .venv/bin/python scale_build.py --corpus rust-rfcs --ns 50,100,200,400,634 --seeds 0

Verification per store (asserts, stops on failure): records==N, parity len(chunks)==N, every target
title resolves to exactly one IRI.
"""
import argparse
import asyncio
import json
import os
import random
import shutil
import subprocess
import time
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

import config
from bootstrap_rust_rfcs import parse_rfc, RFC_DIR
from export_corpus import QUERY, rows_to_chunks
from mcp_client import call_tool
from scale_probe import load_targets

STORES = Path(os.path.expanduser("~/code/moosedev_benches/scale")) / "stores"
EXPORTS = config.BENCH / "scale" / "exports"


def env_for(store: Path) -> dict:
    return {"MOOSEDEV_DATA_DIR": str(store), "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR,
            "MOOSEDEV_NO_AUTOSPAWN": "1", "MOOSEDEV_LLM_BASE_URL": config.LLM_BASE_URL,
            "MOOSEDEV_LLM_API_KEY": config.LLM_API_KEY, "MOOSEDEV_LLM_MODEL": config.NLQ_MODEL}


def start_serve(store: Path, log: Path) -> subprocess.Popen:
    env = {**os.environ, **{k: v for k, v in env_for(store).items() if k != "MOOSEDEV_NO_AUTOSPAWN"}}
    store.mkdir(parents=True, exist_ok=True)
    proc = subprocess.Popen([config.MOOSEDEV_BIN, "--serve"], env=env,
                            stdout=log.open("w"), stderr=subprocess.STDOUT)
    sock = store / "moosedev.sock"
    for _ in range(240):  # up to ~120s for a cold dense-index build
        if sock.exists():
            return proc
        if proc.poll() is not None:
            raise RuntimeError(f"serve exited early ({proc.returncode}); see {log}")
        time.sleep(0.5)
    proc.terminate()
    raise RuntimeError(f"serve never came up; see {log}")


def stop_serve(proc: subprocess.Popen) -> None:
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()


def title_to_file() -> dict[str, dict]:
    """title -> parsed RFC record (parse_rfc derives the 'RFC N: feature' title)."""
    return {rec["title"]: rec for f in sorted(RFC_DIR.glob("*.md")) for rec in [parse_rfc(f)]}


async def write_records(store: Path, recs: list[dict]) -> None:
    params = StdioServerParameters(command=config.MOOSEDEV_BIN, args=["--connect"],
                                   env={**os.environ, **env_for(store)})
    async with stdio_client(params) as (r, w):
        async with ClientSession(r, w) as s:
            await s.initialize()
            for rec in recs:
                await s.call_tool("record_important_decision", {
                    "title": rec["title"], "kind": "ArchitecturalDecision",
                    "description": rec["description"], "status": "accepted"})


def count_records(store: Path) -> int:
    import re
    out = asyncio.run(call_tool(config.MOOSEDEV_BIN, ["--connect"], env_for(store), "sparql", {"query":
        "SELECT (COUNT(DISTINCT ?s) AS ?n) WHERE { GRAPH <https://moosedev.dev/kg/project> "
        "{ ?s <http://www.w3.org/2000/01/rdf-schema#label> ?l } }"}))
    m = re.search(r'"value"\s*:\s*"(\d+)"', out)
    return int(m.group(1)) if m else -1


def export_and_map(store: Path, corpus: str, n: int, seed: int, targets: list[str]):
    out = asyncio.run(call_tool(config.MOOSEDEV_BIN, ["--connect"], env_for(store), "sparql",
                                {"query": QUERY}))
    chunks = rows_to_chunks(out, include_status=True)
    EXPORTS.mkdir(parents=True, exist_ok=True)
    (EXPORTS / f"{corpus}_N{n}_s{seed}.json").write_text(json.dumps(chunks, indent=2))
    t2i: dict[str, list[str]] = {}
    for c in chunks:
        t2i.setdefault(c["title"], []).append(c["iri"])
    return chunks, t2i


def build(corpus: str, ns: list[int], seeds: list[int]) -> None:
    t2f = title_to_file()
    targets = [t["title"] for t in load_targets(corpus)]
    miss = [t for t in targets if t not in t2f]
    assert not miss, f"target titles with no RFC file: {miss}"
    nontargets = [t for t in t2f if t not in set(targets)]
    print(f"corpus={corpus}  targets={len(targets)}  pool={len(nontargets)}  ns={ns}  seeds={seeds}")
    for seed in seeds:
        pool = list(nontargets)
        random.Random(seed).shuffle(pool)  # SAME nested ladder as scale_probe.sweep_b1rag
        for n in ns:
            titles = targets + pool[: max(0, n - len(targets))]
            recs = [t2f[t] for t in titles]
            store = STORES / corpus / f"N{n}_s{seed}" / ".moosedev"
            log = STORES / corpus / f"N{n}_s{seed}" / "serve.log"
            exp = EXPORTS / f"{corpus}_N{n}_s{seed}.json"
            mp = EXPORTS / f"{corpus}_N{n}_s{seed}.targets.json"
            if store.exists() and exp.exists() and mp.exists():
                print(f"  N={n:>4} s={seed}: exists — skip")
                continue
            if store.exists():
                shutil.rmtree(store)
            t0 = time.time()
            proc = start_serve(store, log)
            try:
                asyncio.run(write_records(store, recs))
                cnt = count_records(store)
                chunks, t2i = export_and_map(store, corpus, n, seed, targets)
                assert cnt == len(titles) == len(chunks), \
                    f"N={n}: store={cnt} titles={len(titles)} chunks={len(chunks)}"
                bad = [t for t in targets if len(t2i.get(t, [])) != 1]
                assert not bad, f"N={n}: targets not 1:1 in export: {bad}"
                (EXPORTS / f"{corpus}_N{n}_s{seed}.targets.json").write_text(
                    json.dumps({t: t2i[t][0] for t in targets}, indent=2))
                embedded = "embedded" in log.read_text()
                print(f"  N={n:>4} s={seed}: records={cnt} parity OK 1:1 targets={len(targets)} "
                      f"index={'built' if embedded else 'n/a'} ({time.time()-t0:.0f}s)")
            finally:
                stop_serve(proc)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="rust-rfcs")
    ap.add_argument("--ns", default="50,100,200,400,634")
    ap.add_argument("--seeds", default="0")
    a = ap.parse_args()
    build(a.corpus, [int(x) for x in a.ns.split(",")], [int(x) for x in a.seeds.split(",")])


if __name__ == "__main__":
    main()
