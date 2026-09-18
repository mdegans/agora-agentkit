#!/usr/bin/env python3
"""Hold the two governance-log verifiers to each other on inputs nobody wrote.

The shared vectors say the Rust and Python verifiers agree on the cases
someone thought of. This mutates those vectors — flipped hex, shifted
positions, swapped and duplicated links, missing, unknown and mistyped
fields, inside certificates and out — and asks both verifiers for a
verdict on every mutant. They must agree on all of it: the whole verdict,
or that the input is not a chain at all. A disagreement means one of them
is wrong about a rule, which is the only thing this looks for.

    python3 tools/fuzz_verifiers.py [--count 4000] [--seed 1] [--keep DIR]

Exit status is 0 if and only if there were no disagreements. Deterministic
for a given seed and set of vectors.
"""

import argparse
import copy
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)

import test_vectors  # noqa: E402
import verify_governance_log as govlog  # noqa: E402

REFUSED = "refused"


def paths(value, prefix=()):
    """Every path into `value`, containers included"""
    yield prefix
    if isinstance(value, dict):
        for key in value:
            yield from paths(value[key], prefix + (key,))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            yield from paths(item, prefix + (index,))


def get(value, path):
    for step in path:
        value = value[step]
    return value


def put(root, path, new):
    get(root, path[:-1])[path[-1]] = new


def is_hex(value):
    return (
        isinstance(value, str)
        and len(value) in (64, 128)
        and all(c in "0123456789abcdefABCDEF" for c in value)
    )


def mutate(rng, vector):
    """One change to `vector["links"]`, described"""
    links = vector["links"]
    op = rng.choice(
        ["value"] * 12 + ["delete", "unknown", "swap_links", "drop_link", "dup_link", "graft"]
    )
    if op == "swap_links" and len(links) > 1:
        i, j = rng.sample(range(len(links)), 2)
        links[i], links[j] = links[j], links[i]
        return "swap links %d and %d" % (i, j)
    if op == "drop_link" and len(links) > 1:
        i = rng.randrange(len(links))
        del links[i]
        return "drop link %d" % i
    if op == "dup_link" and links:
        i = rng.randrange(len(links))
        links.insert(rng.randrange(len(links) + 1), copy.deepcopy(links[i]))
        return "duplicate link %d" % i

    everywhere = [p for p in paths(links) if p]
    if not everywhere:
        return "nothing left to change"
    path = rng.choice(everywhere)
    where = "/".join(str(step) for step in path)
    old = get(links, path)
    if op == "delete":
        del get(links, path[:-1])[path[-1]]
        return "delete %s" % where
    if op == "unknown":
        objects = [p for p in paths(links) if isinstance(get(links, p), dict)]
        target = rng.choice(objects)
        get(links, target)["note"] = "fuzz"
        return "unknown field in %s" % "/".join(str(s) for s in target)
    if op == "graft":
        # The same kind of value from somewhere else in the chain: a
        # signature that is somebody's, a hash that is some entry's.
        same = [
            p
            for p in everywhere
            if p != path and type(get(links, p)) is type(old) and get(links, p) != old
        ]
        if same:
            put(links, path, copy.deepcopy(get(links, rng.choice(same))))
            return "graft into %s" % where

    if is_hex(old):
        choice = rng.randrange(4)
        if choice == 0:
            i = rng.randrange(len(old))
            new = old[:i] + "0123456789abcdef"[(int(old[i], 16) + 1) % 16] + old[i + 1 :]
        elif choice == 1:
            new = old.upper()
        elif choice == 2:
            new = old[:-2]
        else:
            new = "00" * (len(old) // 2)
    elif isinstance(old, bool):
        new = not old
    elif isinstance(old, int):
        new = rng.choice(
            [old + 1, old - 1, 0, -1, 2**32, 2**63, 2**64, float(old), str(old), True]
        )
    elif isinstance(old, str):
        new = rng.choice([old + "x", "", old.upper(), None, 7, "routine", "compromise", "genesis"])
    elif old is None:
        new = rng.choice(["0" * 64, 0, "", {}])
    elif isinstance(old, list):
        new = rng.choice([[], old + old, None, {}])
    else:
        new = rng.choice([None, [], {}, "x"])
    put(links, path, new)
    return "%s: %r -> %r" % (where, old, new)


def python_verdict(vector):
    try:
        verdict = test_vectors.observed(test_vectors.run_vector(vector))
    except govlog.InputError:
        return REFUSED
    # The vector files leave an empty `standing` out.
    if not verdict["standing"]:
        del verdict["standing"]
    return verdict


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--count", type=int, default=4000)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--keep", help="write the corpus here and leave it")
    args = parser.parse_args(argv)

    rng = random.Random(args.seed)
    directory = os.path.join(ROOT, "vectors", "govlog")
    seeds = []
    for name in sorted(os.listdir(directory)):
        if name.endswith(".json"):
            with open(os.path.join(directory, name), encoding="utf-8") as f:
                seeds.append((name[:-5], json.load(f)))

    corpus = args.keep or tempfile.mkdtemp(prefix="govlog-fuzz-")
    os.makedirs(corpus, exist_ok=True)
    mutants = []
    for n in range(args.count):
        name, seed = seeds[n % len(seeds)]
        vector = copy.deepcopy(seed)
        what = "; ".join(mutate(rng, vector) for _ in range(rng.choice([1, 1, 1, 2, 3])))
        path = os.path.join(corpus, "%05d_%s.json" % (n, name))
        with open(path, "w", encoding="utf-8") as f:
            json.dump(vector, f)
        mutants.append((path, what, vector))

    env = dict(os.environ, AGORA_FUZZ_DIR=corpus)
    subprocess.run(
        [
            "cargo", "test", "--quiet", "--all-features", "--lib",
            "govlog::vectors::observe_the_fuzz_corpus",
            "--", "--ignored", "--exact",
        ],
        cwd=ROOT, env=env, check=True, stdout=subprocess.DEVNULL,
    )

    disagreements = 0
    refused = 0
    not_ok = 0
    for path, what, vector in mutants:
        with open(path[:-5] + ".rust", encoding="utf-8") as f:
            rust = json.load(f)
        python = python_verdict(vector)
        refused += python == REFUSED
        not_ok += python != REFUSED and not python["ok"]
        if rust != python:
            disagreements += 1
            print("DISAGREEMENT %s\n  %s" % (os.path.basename(path), what))
            if REFUSED in (rust, python):
                print("  rust: %s\n  python: %s" % (
                    rust if rust == REFUSED else "a verdict",
                    python if python == REFUSED else "a verdict",
                ))
            else:
                for key in sorted(set(rust) | set(python)):
                    if rust.get(key) != python.get(key):
                        print("  %s:\n    rust   %s\n    python %s" % (
                            key, json.dumps(rust.get(key)), json.dumps(python.get(key))
                        ))
    print(
        "%d mutants from %d vectors (seed %d): %d refused by both, %d not ok, "
        "%d still ok; %d disagreements"
        % (
            len(mutants), len(seeds), args.seed, refused - 0, not_ok,
            len(mutants) - refused - not_ok, disagreements,
        )
    )
    if not args.keep and not disagreements:
        shutil.rmtree(corpus)
    elif disagreements:
        print("corpus kept in %s" % corpus)
    return 1 if disagreements else 0


if __name__ == "__main__":
    sys.exit(main())
