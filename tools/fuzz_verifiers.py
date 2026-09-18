#!/usr/bin/env python3
"""Hold the two governance-log verifiers to each other on inputs nobody wrote.

The shared vectors say the Rust and Python verifiers agree on the cases
someone thought of. This mutates those vectors — flipped hex, shifted
positions, swapped and duplicated links, missing, unknown and mistyped
fields, inside certificates and out — and asks both verifiers for a
verdict on every mutant. They must agree on all of it: the whole verdict,
or that the input is not a chain at all. A disagreement means one of them
is wrong about a rule, which is the only thing this looks for.

Half the mutants are left as they are, which exercises the envelope: almost
any change breaks a hash or a signature, and both verifiers must say so.
The other half change a signed payload — an amendment, a rotation, a root
certificate's statement — and are then RE-SIGNED, link by link and root
signature by root signature, with the fixed test keys the vectors are built
from. Those are the mutants that get past the hashes to the rules about
what a payload may say, which is where two implementations can quietly
differ. (The keys are test fixtures: `[n; 32]` for the chain, `[0xA0 + n;
32]` for the throwaway roots. None of them signs anything real.)

    python3 tools/fuzz_verifiers.py [--count 4000] [--seed 1] [--keep DIR]

Exit status is 0 if and only if there were no disagreements. Deterministic
for a given seed and set of vectors.
"""

import argparse
import copy
import hashlib
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


# ---------------------------------------------------------------------------
# Signing, with the vectors' fixed keys
# ---------------------------------------------------------------------------


def _compress(point):
    x, y, z, _ = point
    inverse = pow(z, govlog._P - 2, govlog._P)
    x, y = x * inverse % govlog._P, y * inverse % govlog._P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


class Signer:
    """An Ed25519 signing key from its 32-byte seed (RFC 8032 §5.1.5-6)"""

    def __init__(self, seed):
        digest = hashlib.sha512(seed).digest()
        scalar = bytearray(digest[:32])
        scalar[0] &= 248
        scalar[31] &= 127
        scalar[31] |= 64
        self.scalar = int.from_bytes(scalar, "little")
        self.prefix = digest[32:]
        self.public = _compress(govlog._point_mul(self.scalar, govlog._G))

    def sign(self, message):
        r = int.from_bytes(hashlib.sha512(self.prefix + message).digest(), "little") % govlog._L
        big_r = _compress(govlog._point_mul(r, govlog._G))
        k = int.from_bytes(
            hashlib.sha512(big_r + self.public + message).digest(), "little"
        ) % govlog._L
        return big_r + ((r + k * self.scalar) % govlog._L).to_bytes(32, "little")


SIGNERS = {}
for _seed in [bytes([n]) * 32 for n in range(1, 6)] + [bytes([0xA0 + n]) * 32 for n in (1, 2)]:
    _signer = Signer(_seed)
    SIGNERS[_signer.public.hex()] = _signer


def who_signed(link):
    """The test key whose signature is on `link`, if it is one of ours"""
    a = link["attestation"]
    try:
        message = govlog.signed_message(
            bytes.fromhex(a["entry_hash"]), govlog.parse_timestamp(a["signed_at"])[1]
        )
        signature = bytes.fromhex(a["signature"])
    except (govlog.InputError, ValueError, TypeError, KeyError):
        return None
    for public, signer in SIGNERS.items():
        if govlog.ed25519_verify(bytes.fromhex(public), message, signature):
            return signer
    return None


def resign_certificates(data, signed_by_root):
    """Root signatures over whatever the statements say now, by whichever
    of our roots had signed them before"""
    if not isinstance(data, dict):
        return
    for name in ("certificate", "outgoing_certificate"):
        certificate = data.get(name)
        signers = signed_by_root.get(name) or []
        if not isinstance(certificate, dict) or not signers:
            continue
        try:
            parsed = govlog._parse_certificate(
                dict(certificate, signatures=[]), name
            )
            message = govlog.root_signed_bytes(parsed["statement"])
        except (govlog.InputError, TypeError, KeyError, AttributeError):
            continue
        certificate["signatures"] = [
            {"root_key": public, "signature": SIGNERS[public].sign(message).hex()}
            for public in signers
        ]


def root_signers(data):
    """Which of our roots validly signed each certificate in `data`"""
    found = {}
    if not isinstance(data, dict):
        return found
    for name in ("certificate", "outgoing_certificate"):
        certificate = data.get(name)
        try:
            parsed = govlog._parse_certificate(certificate, name)
        except (govlog.InputError, TypeError, KeyError, AttributeError):
            continue
        message = govlog.root_signed_bytes(parsed["statement"])
        found[name] = [
            s["root_key"]
            for s in parsed["signatures"]
            if s["root_key"] in SIGNERS
            and govlog.ed25519_verify(
                bytes.fromhex(s["root_key"]), message, bytes.fromhex(s["signature"])
            )
        ]
    return found


def mutate_payload(rng, vector):
    """One change inside a signed payload, then everything re-signed so
    that the change is the only thing wrong. None if there is no payload
    to change or the chain is not one of ours to sign."""
    links = vector["links"]
    carrying = [i for i, l in enumerate(links) if isinstance(l.get("data"), (dict, list))]
    signers = [who_signed(l) for l in links]
    if not carrying:
        return None
    index = rng.choice(carrying)
    link = links[index]
    roots_before = root_signers(link["data"])

    holder = {"links": [{"data": link["data"]}]}
    what = None
    for _ in range(8):  # a mutation of the payload itself, not of the wrapper
        trial = copy.deepcopy(holder)
        described = mutate(rng, trial, structural=False)
        wrapper = trial["links"][0] if len(trial["links"]) == 1 else None
        if (
            isinstance(wrapper, dict)
            and set(wrapper) == {"data"}
            and wrapper["data"] is not None
            and wrapper["data"] != link["data"]
        ):
            holder, what = trial, described
            break
    if what is None:
        return None
    link["data"] = holder["links"][0]["data"]
    if rng.random() < 0.7:
        resign_certificates(link["data"], roots_before)

    previous = links[index - 1]["attestation"]["entry_hash"] if index else None
    for i in range(index, len(links)):
        current, a = links[i], links[i]["attestation"]
        if i == index:
            try:
                a["data_hash"] = govlog.data_hash(current["data"])
            except Exception:  # not JSON a verifier can hash; leave it broken
                return None
        else:
            a["prev_hash"] = previous
        try:
            a["entry_hash"] = govlog.entry_hash(current)
            if signers[i] is not None:
                a["signature"] = signers[i].sign(
                    govlog.signed_message(
                        bytes.fromhex(a["entry_hash"]),
                        govlog.parse_timestamp(a["signed_at"])[1],
                    )
                ).hex()
        except (govlog.InputError, ValueError, TypeError, KeyError):
            return None
        previous = a["entry_hash"]
    return "re-signed payload of link %d: %s" % (index, what.replace("0/data/", "", 1))


def mutate(rng, vector, structural=True):
    """One change to `vector["links"]`, described"""
    links = vector["links"]
    op = rng.choice(
        ["value"] * 12
        + ["delete", "delete", "unknown", "graft", "retype", "retype"]
        + (["swap_links", "drop_link", "dup_link"] if structural else [])
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
    if op == "retype":
        # The same content in another JSON shape: serde reads a struct from
        # an array of its fields, and Python will not.
        if isinstance(old, dict):
            new = rng.choice([list(old.values()), [], [old], None])
        elif isinstance(old, list):
            new = rng.choice([{str(i): v for i, v in enumerate(old)}, {}, old[:1] or None])
        else:
            new = rng.choice([[old], {"value": old}, None])
        put(links, path, new)
        return "retype %s: %r -> %r" % (where, old, new)
    if op == "unknown":
        objects = [p for p in paths(links) if isinstance(get(links, p), dict)]
        if not objects:
            return "no object left to add a field to"
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
            # 2**64 and the float are numbers governance data never holds:
            # both verifiers must report them, and neither may hash them —
            # how a float is written is the one part of canonical JSON no
            # two libraries agree on.
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
    except Exception as e:  # a crash is a finding, never a verdict
        return "crashed: %s: %s" % (type(e).__name__, e)
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
        what = mutate_payload(rng, vector) if n % 2 else None
        if what is None:
            what = "; ".join(
                mutate(rng, vector) for _ in range(rng.choice([1, 1, 1, 2, 3]))
            )
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
        not_ok += isinstance(python, dict) and not python["ok"]
        if rust != python:
            disagreements += 1
            print("DISAGREEMENT %s\n  %s" % (os.path.basename(path), what))
            if isinstance(rust, str) or isinstance(python, str):
                print("  rust: %s\n  python: %s" % (
                    rust if isinstance(rust, str) else "a verdict",
                    python if isinstance(python, str) else "a verdict",
                ))
            else:
                for r, p in zip(rust.get("entries", []), python.get("entries", [])):
                    for key in sorted(set(r) | set(p)):
                        if r.get(key) != p.get(key):
                            print("  %s.%s: rust %s, python %s" % (
                                r.get("id"), key, json.dumps(r.get(key)), json.dumps(p.get(key))
                            ))
                for key in sorted((set(rust) | set(python)) - {"entries"}):
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
