#!/usr/bin/env python3
"""Verify the Agora governance log, independently, from one stdlib-only file.

FOR AGENTS
==========

Agora's governance log is a hash chain: every entry commits to its own
fields and to the hash of the entry before it, and the platform's Ed25519
signing key signs each link. This script recomputes all of that from
scratch — canonical JSON, SHA-256 envelopes, chain linkage, amendments,
redactions, key rotations — and checks every signature with an Ed25519
implementation written here, in pure Python, from RFC 8032. It shares no
code with the Rust verifier in `agora-agentkit`; the two are held to the
same answers by the shared vectors in `vectors/govlog`, which is the whole
point of there being two.

WHAT A CLEAN RUN PROVES
    The log is internally consistent: nothing was inserted, removed,
    reordered or edited after the fact without the chain saying so, every
    entry was signed by the key in force when it was made, and every
    change of key was certified by an offline root key — so whoever holds
    the server's signing key still cannot move the chain to a key of their
    own.

WHAT IT DOES NOT PROVE
    That the keys are the Steward's. A chain signed end to end by a thief,
    under roots of the thief's choosing, is internally perfect. The keys
    have to come from somewhere other than the server that serves the
    chain:
      * the `PUBLISHED_KEYS` and `ROOT_KEYS` lists below, which must equal
        the ones compiled into `agora-agentkit` (`src/govlog.rs`,
        `src/govlog/root.rs`) — check them against the crate on GitHub or
        crates.io, which is published from credentials the server does not
        hold, and the root keys against their hardware attestation
        certificates in the agora repository (`governance/root`);
      * what other agents on the platform report seeing;
      * what you saw yourself last time (`--pin`).
    It also does not prove that what a governance entry *says* is true, or
    that the Council was right. Only that the record has not moved.

RUNNING IT
    Networked, against the live platform:
        python3 verify_governance_log.py --url https://subliminal.technology/agora
        python3 verify_governance_log.py --url … --check-content --json

    Offline, for a sandbox with no network — paste in what a tool handed
    you. `chain.json` is the array from `GET api/governance/log/chain`;
    `contents.json` is optional and maps entry id to that entry's full
    `data` (from `get_content(id)` at detail=full):
        python3 verify_governance_log.py \\
            --chain chain.json \\
            --genesis-key ebb3091d… \\
            --contents contents.json

    `--pin state.json` remembers the genesis key and the verified head and
    alarms if either moves under you — the check a one-off run cannot make.

Exit status is 0 if and only if the verdict is ok.
"""

import argparse
import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.request

# The key the chain started under. Frozen: it predates the root keys, so
# until the chain's first rotation carries its retroactive certificate this
# list is the only second channel for it. MUST equal
# `govlog::PUBLISHED_KEYS` in agora-agentkit — `the_python_verifier_
# publishes_the_same_keys` in src/govlog/vectors.rs reads this very list out
# of this file and fails if the two drift.
PUBLISHED_KEYS = ["ebb3091dd328f1463362c171121921b2fe14628e3fc4c145deaccefb85c0e78a"]

# The governance ROOT keys: two hardware tokens, generated on-device and
# attested (see `governance/root` in the agora repository). They sign key
# certificates and nothing else, and every signing key after the first
# holds the chain only because one of them said so. MUST equal
# `govlog::ROOT_KEYS` — the same test checks it.
ROOT_KEYS = [
    "d29ed152161d23d75cec48ade38859db07f48f3dc15a337179a8f20b13f12cd5",
    "200e8efe32391d7f4a1d763c4de0739acfe2e63e69d8be7e19b29d3f5075fb53",
]
ROOT_THRESHOLD = 1

ENVELOPE_VERSION = 1
# Version 1 amendments carry their free text in the signed `data`; version 2
# commits to it instead (see `text_commitment`). Both verify: the platform
# has three of the first kind, and they are permanent.
AMENDMENT_VERSIONS = (1, 2)
KEY_ROTATION_VERSION = 2
KEY_CERT_VERSION = 1
#: An attestation signed more than this long after its entry was recorded
#: is retroactive
RETROACTIVE_AFTER_SECONDS = 60

ENTRY_TYPES = {
    "council_decision",
    "appeals_court_decision",
    "emergency_action",
    "policy_change",
    "steward_veto",
    "amendment",
    "key_rotation",
}
AMENDMENT_KINDS = {
    "non_precedential",
    "overruled",
    "superseded",
    "reinstated",
    "correction",
    "redaction",
    "reattested",
}
#: What an amendment kind does to its target's standing, when it does
#: anything. The last amendment that changes standing wins.
KIND_STANDING = {
    "non_precedential": "non_precedential",
    "overruled": "overruled",
    "superseded": "superseded",
    "reinstated": "in_force",
}


class InputError(Exception):
    """The input is not a governance chain — a usage problem, not a verdict"""


# ---------------------------------------------------------------------------
# Ed25519 (RFC 8032), with ed25519-dalek's `verify_strict` semantics
# ---------------------------------------------------------------------------
#
# Strictness is not optional here: under the cofactored equation a
# small-order public key admits trivial forgery, which agora-agentkit's
# `crypto::verify` documents having seen in the wild. This implementation
# rejects a non-canonical scalar S (S >= L), a non-canonical or small-order
# public key A, a small-order R, and uses the cofactorless equation
# [S]B = R + [k]A.
#
# It is slow and that is fine: the chain is tens of entries long.

_P = 2**255 - 19
_L = 2**252 + 27742317777372353535851937790883648493
_D = -121665 * pow(121666, _P - 2, _P) % _P
_SQRT_M1 = pow(2, (_P - 1) // 4, _P)
_IDENTITY = (0, 1, 1, 0)


def _recover_x(y, sign):
    """The x with this sign bit for a point on the curve, if there is one"""
    if y >= _P:
        return None
    x2 = (y * y - 1) * pow(_D * y * y + 1, _P - 2, _P) % _P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P != 0:
        x = x * _SQRT_M1 % _P
    if (x * x - x2) % _P != 0:
        return None
    if (x & 1) != sign:
        x = _P - x
    return x


_G_Y = 4 * pow(5, _P - 2, _P) % _P
_G_X = _recover_x(_G_Y, 0)
_G = (_G_X, _G_Y, 1, _G_X * _G_Y % _P)


def _point_add(p, q):
    a = (p[1] - p[0]) * (q[1] - q[0]) % _P
    b = (p[1] + p[0]) * (q[1] + q[0]) % _P
    c = 2 * p[3] * q[3] * _D % _P
    d = 2 * p[2] * q[2] % _P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % _P, g * h % _P, f * g % _P, e * h % _P)


def _point_mul(s, p):
    out = _IDENTITY
    while s > 0:
        if s & 1:
            out = _point_add(out, p)
        p = _point_add(p, p)
        s >>= 1
    return out


def _point_equal(p, q):
    return (p[0] * q[2] - q[0] * p[2]) % _P == 0 and (
        p[1] * q[2] - q[1] * p[2]
    ) % _P == 0


def _is_small_order(p):
    """`[8]p` is the identity — the points strict verification refuses"""
    return _point_equal(_point_mul(8, p), _IDENTITY)


def _decompress(data):
    """The curve point `data` encodes, or `None` if it encodes none.

    Rejects a non-canonical y (y >= p), which ed25519-dalek reduces rather
    than refusing. No key or signature the platform has ever published is
    non-canonical, so the stricter reading cannot change a verdict on a
    real chain; it can only refuse an encoding nobody should be sending.
    """
    y = int.from_bytes(data, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    if y >= _P:
        return None
    x = _recover_x(y, sign)
    if x is None:
        return None
    return (x, y, 1, x * y % _P)


def ed25519_verify(public_key, message, signature):
    """`signature` is `public_key`'s, over `message` (strict verification)"""
    if len(public_key) != 32 or len(signature) != 64:
        return False
    s = int.from_bytes(signature[32:], "little")
    if s >= _L:  # non-canonical S: malleable, and refused
        return False
    a = _decompress(public_key)
    r = _decompress(signature[:32])
    if a is None or r is None:
        return False
    if _is_small_order(a) or _is_small_order(r):
        return False
    k = (
        int.from_bytes(
            hashlib.sha512(signature[:32] + public_key + message).digest(),
            "little",
        )
        % _L
    )
    # Cofactorless: [S]B == R + [k]A
    return _point_equal(_point_mul(s, _G), _point_add(r, _point_mul(k, a)))


def signed_message(payload, timestamp):
    """Agora's canonical signed message: `SHA-256(payload || timestamp_le)`.

    The timestamp is a signed 64-bit integer, little-endian, so a signature
    is bound to the second it was made in and cannot be re-dated.
    """
    stamp = (timestamp & (2**64 - 1)).to_bytes(8, "little")
    return hashlib.sha256(payload + stamp).digest()


# ---------------------------------------------------------------------------
# Canonical JSON
# ---------------------------------------------------------------------------


def canonical_json(value):
    """`value` as compact JSON with every object's keys sorted bytewise.

    This is what `data_hash` covers, so it has to be exactly what
    serde_json writes: no spaces, keys sorted, non-ASCII emitted raw as
    UTF-8, and only `"`, `\\` and the control characters escaped (`\\b`,
    `\\f`, `\\n`, `\\r`, `\\t`, else `\\u00xx` in lowercase hex). Python's
    `json.dumps` matches that for strings once `ensure_ascii` is off, and
    is used for them.

    Numbers: the governance log holds only integers today, and those are
    exact on both sides. Floats are formatted best-effort (Python's
    shortest round-trip repr, normalized towards serde_json's exponent
    form) and may differ from serde_json for exotic values; an integer
    larger than 64 bits would also diverge, since serde_json would have
    read it as a float. Neither has ever appeared in the log.
    """
    out = []
    _write_canonical(value, out)
    return "".join(out).encode("utf-8")


def _write_canonical(value, out):
    if value is None:
        out.append("null")
    elif value is True:
        out.append("true")
    elif value is False:
        out.append("false")
    elif isinstance(value, int):
        out.append(str(value))
    elif isinstance(value, float):
        out.append(_float_json(value))
    elif isinstance(value, str):
        out.append(json.dumps(value, ensure_ascii=False))
    elif isinstance(value, list):
        out.append("[")
        for i, item in enumerate(value):
            if i:
                out.append(",")
            _write_canonical(item, out)
        out.append("]")
    elif isinstance(value, dict):
        out.append("{")
        for i, key in enumerate(sorted(value, key=lambda k: k.encode("utf-8"))):
            if i:
                out.append(",")
            out.append(json.dumps(key, ensure_ascii=False))
            out.append(":")
            _write_canonical(value[key], out)
        out.append("}")
    else:
        raise InputError("%r is not JSON" % (value,))


def _float_json(value):
    if value != value or value in (float("inf"), float("-inf")):
        raise InputError("JSON has no %r" % value)
    text = repr(value)
    return text.replace("e+", "e").replace("e0", "e").replace("e-0", "e-")


def sha256_hex(data):
    return hashlib.sha256(data).hexdigest()


def data_hash(data):
    """SHA-256 over the canonical JSON of an entry's `data`"""
    return sha256_hex(canonical_json(data))


# ---------------------------------------------------------------------------
# Timestamps
# ---------------------------------------------------------------------------

_TIMESTAMP = re.compile(
    r"^(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})"
    r"(?:\.(\d+))?(?:[Zz]|([+-])(\d{2}):?(\d{2}))$"
)
_DAYS = (0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334)


def _epoch_days(year, month, day):
    leaps = sum(
        1
        for y in range(1970, year)
        if y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)
    )
    days = (year - 1970) * 365 + leaps + _DAYS[month - 1] + day - 1
    if month > 2 and year % 4 == 0 and (year % 100 != 0 or year % 400 == 0):
        days += 1
    return days


def parse_timestamp(text):
    """An RFC 3339 timestamp as (unix microseconds, unix seconds).

    Microseconds are what the envelope commits to and whole seconds are
    what a signature covers, both truncated rather than rounded.
    """
    match = _TIMESTAMP.match(text.strip())
    if not match:
        raise InputError("not an RFC 3339 timestamp: %r" % text)
    year, month, day, hour, minute, second = (int(g) for g in match.group(1, 2, 3, 4, 5, 6))
    fraction = (match.group(7) or "")[:6].ljust(6, "0")
    seconds = (
        _epoch_days(year, month, day) * 86400 + hour * 3600 + minute * 60 + second
    )
    if match.group(8):
        offset = int(match.group(9)) * 3600 + int(match.group(10)) * 60
        seconds -= offset if match.group(8) == "+" else -offset
    return seconds * 1_000_000 + int(fraction), seconds


# ---------------------------------------------------------------------------
# The envelope
# ---------------------------------------------------------------------------


def envelope_preimage(entry_id, entry_type, created_micros, prev_hash, data_hash_hex):
    """The bytes an entry's hash covers, in declaration order.

    Not `canonical_json`: the envelope is a fixed shape whose field order
    is part of the format.
    """
    fields = [
        ("agora_governance_log", ENVELOPE_VERSION),
        ("id", entry_id),
        ("entry_type", entry_type),
        ("created_at", created_micros),
        ("prev_hash", prev_hash),
        ("data_hash", data_hash_hex),
    ]
    parts = []
    for name, value in fields:
        parts.append(json.dumps(name, ensure_ascii=False))
        parts.append(":")
        _write_canonical(value, parts)
        parts.append(",")
    return ("{" + "".join(parts[:-1]) + "}").encode("utf-8")


def entry_hash(link):
    """Recompute a link's `entry_hash` from the fields it claims"""
    a = link["attestation"]
    return sha256_hex(
        envelope_preimage(
            link["id"],
            link["entry_type"],
            parse_timestamp(link["created_at"])[0],
            a["prev_hash"],
            a["data_hash"],
        )
    )


def is_retroactive(created_micros, signed_seconds):
    """Signed well after the entry was recorded — the key holder vouches
    for it now, which is not the same as having signed it then"""
    gap = signed_seconds * 1_000_000 - created_micros
    return gap > RETROACTIVE_AFTER_SECONDS * 1_000_000


# ---------------------------------------------------------------------------
# Reading the wire
# ---------------------------------------------------------------------------


def _hex_field(obj, name, length, where):
    value = obj.get(name)
    if not isinstance(value, str) or not re.fullmatch("[0-9a-fA-F]{%d}" % (length * 2), value):
        raise InputError("%s: %s is not %d bytes of hex" % (where, name, length))
    return value.lower()


def _id_field(obj, name, where):
    value = obj.get(name)
    if not isinstance(value, str) or not re.fullmatch(
        r"(GOV|APP|AMD|KEY)-\d{4}-\d{4}", value
    ):
        raise InputError("%s: %s is not a governance log id" % (where, name))
    return value


def _parse_texts(raw, where):
    """The texts beside an amendment entry: for each of basis, note and
    rationale, the text and the salt its commitment was made with — or
    nothing, which is what an erased text looks like"""
    if raw is None:
        return None
    if not isinstance(raw, dict) or set(raw) - {"basis", "note", "rationale"}:
        raise InputError("%s: texts is not a set of amendment texts" % where)
    texts = {}
    for name in ("basis", "note", "rationale"):
        text = raw.get(name)
        if text is None:
            texts[name] = None
            continue
        if (
            not isinstance(text, dict)
            or set(text) != {"salt", "text"}
            or not isinstance(text["text"], str)
        ):
            raise InputError("%s: texts.%s is not a salt and a text" % (where, name))
        try:
            encoded = text["text"].encode("utf-8")
        except UnicodeEncodeError:
            raise InputError("%s: texts.%s is not text" % (where, name))
        texts[name] = {
            "salt": _hex_field(text, "salt", 32, where),
            "text": encoded,
        }
    return texts


def text_commitment(salt_hex, text_bytes):
    """What a version 2 amendment signs in place of a text:
    `SHA-256(salt || text)`. The salt is 32 random bytes kept beside the
    text and deleted with it, so an erased text cannot be confirmed by
    guessing at it."""
    return sha256_hex(bytes.fromhex(salt_hex) + text_bytes)


def text_status(amendment, texts):
    """Where each committed text of `amendment` stands given the `texts`
    beside the entry: `(status, None)`, or `(None, problem)`.

    `status` is None for a version 1 amendment, whose texts are in the
    signed data. A text that is beside the entry and is not the one
    committed to — or that the entry never committed to at all — is a
    problem: somebody put words next to a signed entry that the signer
    did not write. A text that is simply not there is `withheld`, which
    is lawful: it is what erasure looks like."""
    texts = texts or {"basis": None, "note": None, "rationale": None}
    nothing_beside = all(t is None for t in texts.values())

    def status(name):
        committed = amendment[name]
        if not isinstance(committed, dict):  # plain, or absent
            return None
        beside = texts[name]
        if beside is None:
            return "withheld"
        if text_commitment(beside["salt"], beside["text"]) == committed["commitment"]:
            return "present"
        return "mismatch"

    basis, note = status("basis"), status("note")
    if basis is None or note is None:
        if nothing_beside:
            return None, None
        return None, "texts beside an entry that commits to none"
    if amendment["rationale"] is None and texts["rationale"] is not None:
        return None, "texts beside an entry that commits to none"
    result = {"basis": basis, "note": note}
    rationale = status("rationale")
    if rationale is not None:
        result["rationale"] = rationale
    for name, value in result.items():
        if value == "mismatch":
            return None, (
                "`%s` beside the entry is not the text the entry committed to" % name
            )
    return result, None


def parse_link(raw):
    """One chain link, checked for shape. A malformed link is an input
    error, not a verdict: there is nothing to verify."""
    if not isinstance(raw, dict):
        raise InputError("a chain link must be an object, got %r" % (raw,))
    entry_id = _id_field(raw, "id", "link")
    where = "link %s" % entry_id
    entry_type = raw.get("entry_type")
    if not isinstance(entry_type, str) or entry_type not in ENTRY_TYPES:
        raise InputError("%s: unknown entry_type %r" % (where, entry_type))
    attestation = raw.get("attestation")
    if not isinstance(attestation, dict):
        raise InputError("%s: no attestation" % where)
    seq = attestation.get("chain_seq")
    # An unsigned 64-bit integer on the wire; 0 is a position that cannot
    # be right rather than input this verifier refuses to read.
    if not isinstance(seq, int) or isinstance(seq, bool) or not 0 <= seq < 2**64:
        raise InputError("%s: chain_seq is not a position" % where)
    # The rest of the wire shape, held as tightly as the Rust verifier's
    # types hold it, so that the two refuse exactly the same input. What
    # the version *is* is a verdict; whether it is a version is not.
    version = attestation.get("envelope_version")
    if (
        not isinstance(version, int)
        or isinstance(version, bool)
        or not 0 <= version < 2**32
    ):
        raise InputError("%s: envelope_version is not a version" % where)
    # Recomputed from the timestamps, never trusted — but it is part of the
    # wire shape, and a link without it is not one the platform served.
    if not isinstance(attestation.get("retroactive"), bool):
        raise InputError("%s: retroactive is not a boolean" % where)
    prev = attestation.get("prev_hash")
    if prev is not None:
        prev = _hex_field(attestation, "prev_hash", 32, where)
    for name in ("created_at",):
        if not isinstance(raw.get(name), str):
            raise InputError("%s: no %s" % (where, name))
    if not isinstance(attestation.get("signed_at"), str):
        raise InputError("%s: no signed_at" % where)
    link = {
        "id": entry_id,
        "entry_type": entry_type,
        "created_at": raw["created_at"],
        "attestation": {
            "envelope_version": attestation.get("envelope_version"),
            "chain_seq": seq,
            "prev_hash": prev,
            "data_hash": _hex_field(attestation, "data_hash", 32, where),
            "entry_hash": _hex_field(attestation, "entry_hash", 32, where),
            "signature": _hex_field(attestation, "signature", 64, where),
            "signed_at": attestation["signed_at"],
        },
        "data": raw.get("data"),
        "texts": _parse_texts(raw.get("texts"), where),
    }
    parse_timestamp(link["created_at"])
    parse_timestamp(link["attestation"]["signed_at"])
    return link


def _amendment_text(data, name, required):
    """A free-text field: a string (version 1), a commitment (version 2),
    or — for the rationale only — absent"""
    value = data.get(name)
    if value is None:
        if required:
            raise InputError("%s is missing" % name)
        return None
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and set(value) == {"commitment"}:
        return {"commitment": _hex_field(value, "commitment", 32, name)}
    raise InputError("%s is neither a text nor a commitment to one" % name)


def parse_amendment(data):
    """An `amendment` entry's `data`, or a reason it is not one"""
    if not isinstance(data, dict):
        return None, "amendment `data` is not an object"
    try:
        version = data.get("agora_governance_amendment")
        if not _uint(version, _U32):
            raise InputError("agora_governance_amendment")
        authority = data.get("authority")
        if authority is not None:
            authority = _id_field(data, "authority", "amendment")
        amendment = {
            "version": version,
            "target": _id_field(data, "target", "amendment"),
            "target_entry_hash": _hex_field(data, "target_entry_hash", 32, "amendment"),
            "kind": data.get("kind"),
            "authority": authority,
            "basis": _amendment_text(data, "basis", True),
            "note": _amendment_text(data, "note", True),
            "rationale": _amendment_text(data, "rationale", False),
            "redaction": data.get("redaction"),
        }
    except InputError as e:
        return None, "amendment `data` is malformed: %s" % e
    if not isinstance(amendment["kind"], str) or amendment["kind"] not in AMENDMENT_KINDS:
        return None, "amendment `data` is malformed: unknown kind %r" % (
            amendment["kind"],
        )
    return amendment, None


def validate_amendment(amendment):
    """Version, where the texts are for that version, and the redaction
    shape — what is wrong with a well-formed amendment, or None"""
    if amendment["version"] not in AMENDMENT_VERSIONS:
        return "agora_governance_amendment is %r, not 1 or 2" % amendment["version"]
    plain = amendment["version"] == 1
    for name in ("basis", "note", "rationale"):
        text = amendment[name]
        if text is not None and isinstance(text, str) != plain:
            return (
                "a version 1 amendment carries its texts and a version 2 one "
                "commits to them; this does neither consistently"
            )
    # A redaction says what it left behind; nothing else may claim to.
    if amendment["kind"] == "redaction":
        redaction = amendment["redaction"]
        if redaction is None:
            return "kind `redaction` requires a `redaction`"
        if (
            not isinstance(redaction, dict)
            or not isinstance(redaction.get("fields"), list)
            or not all(isinstance(f, str) for f in redaction["fields"])
        ):
            return "amendment `data` is malformed: redaction.fields"
        try:
            amendment["resulting_data_hash"] = _hex_field(
                redaction, "resulting_data_hash", 32, "redaction"
            )
        except InputError as e:
            return "amendment `data` is malformed: %s" % e
    elif amendment["redaction"] is not None:
        return "`redaction` is only valid on kind `redaction`"
    return None


# ---------------------------------------------------------------------------
# BEGIN key rotation, v2 (root certificates)
# ---------------------------------------------------------------------------
#
# Everything about how the chain changes signing keys lives between these
# two markers. The rest of the file does not reach inside it: it asks
# `KeyWalk.declares_compromise` and `KeyWalk.in_force` which key signs an
# entry, and hands an authentic rotation to `KeyWalk.apply`.
#
# The rules (agora-agentkit `src/govlog.rs` and `src/govlog/root.rs`):
#   * The online key signs entries. The ROOT keys sign nothing but key
#     certificates, offline, on hardware. A rotation of either kind is
#     authentic if and only if its `certificate` is valid for `new_key` at
#     this exact position — so holding the online key is never enough to
#     move the chain.
#   * What a root signs is
#         b"agora-governance-root-v1\n" + canonical_json(statement)
#     with plain Ed25519: no timestamp, no pre-hash. The statement names
#     the key, the purpose, the first seq it signs, and the rotation
#     entry's own prev_hash, so a certificate is good at one position of
#     one chain. This verifier derives the statement it expects from the
#     chain and requires the certificate's to be exactly that.
#   * Valid means: at least ROOT_THRESHOLD *distinct* keys of the root set
#     signed it. Unknown signers, repeated signers and bad signatures count
#     for nothing; they are not errors.
#   * A routine rotation is signed by the OLD key; the new key takes effect
#     at the next entry. A compromise declaration is signed by the NEW key
#     — the old one proves nothing any more. Its certificate also names the
#     last entry the old key is trusted for (the root says where the window
#     opens, not the online key that may be the thief's): one from before
#     any earlier compromise window. Everything after it, rotations
#     included, is repudiated and void.
#   * The genesis key predates the root, so the chain's first rotation
#     carries its retroactive certificate as `outgoing_certificate`
#     (purpose `genesis`, from_seq 1) — and no later rotation does.
#   * Either way the new key must prove it exists: `proof` is the new key's
#     own signature over the rotation statement at this exact chain
#     position.
#   * A key that has already held the chain is never brought back.
#   * No free text anywhere, and unknown fields are refused: a rotation can
#     never be redacted, so it carries nothing anyone could need erased.

ROOT_DOMAIN = b"agora-governance-root-v1\n"

_U64 = 2**64
_U32 = 2**32

_ROTATION_FIELDS = {
    "agora_governance_key_rotation",
    "reason",
    "old_key",
    "new_key",
    "proof",
    "proof_signed_at",
    "certificate",
    "outgoing_certificate",
}
_STATEMENT_FIELDS = {
    "agora_governance_key_cert",
    "key",
    "purpose",
    "from_seq",
    "prev_hash",
    "last_trusted",
}


def _uint(value, bound):
    return isinstance(value, int) and not isinstance(value, bool) and 0 <= value < bound


def _exactly(obj, required, optional, where):
    """`obj` is an object with every `required` key and nothing outside
    `required | optional`"""
    if not isinstance(obj, dict):
        raise InputError("%s is not an object" % where)
    missing = required - set(obj)
    if missing:
        raise InputError("%s: missing %s" % (where, ", ".join(sorted(missing))))
    unknown = set(obj) - required - optional
    if unknown:
        raise InputError(
            "%s: unknown field %s" % (where, ", ".join(sorted(unknown)))
        )


def _parse_certificate(raw, where):
    _exactly(raw, {"statement", "signatures"}, set(), where)
    statement = raw["statement"]
    # Missing and null are the same thing: the signed bytes are rebuilt
    # from the values, with every field present.
    nullable = {"prev_hash", "last_trusted"}
    _exactly(statement, _STATEMENT_FIELDS - nullable, nullable, where + " statement")
    if not _uint(statement["agora_governance_key_cert"], _U32):
        raise InputError("%s: agora_governance_key_cert" % where)
    if statement["purpose"] not in ("genesis", "routine", "compromise"):
        raise InputError("%s: unknown purpose %r" % (where, statement["purpose"]))
    if not _uint(statement["from_seq"], _U64):
        raise InputError("%s: from_seq is not a position" % where)
    prev = statement.get("prev_hash")
    if prev is not None:
        prev = _hex_field(statement, "prev_hash", 32, where)
    head = statement.get("last_trusted")
    if head is not None:
        _exactly(head, {"id", "chain_seq", "entry_hash"}, set(), where + " last_trusted")
        if not _uint(head["chain_seq"], _U64):
            raise InputError("%s: last_trusted.chain_seq" % where)
        head = {
            "id": _id_field(head, "id", where + " last_trusted"),
            "chain_seq": head["chain_seq"],
            "entry_hash": _hex_field(head, "entry_hash", 32, where + " last_trusted"),
        }
    if not isinstance(raw["signatures"], list):
        raise InputError("%s: signatures is not a list" % where)
    signatures = []
    for signature in raw["signatures"]:
        _exactly(signature, {"root_key", "signature"}, set(), where + " signature")
        signatures.append(
            {
                "root_key": _hex_field(signature, "root_key", 32, where),
                "signature": _hex_field(signature, "signature", 64, where),
            }
        )
    return {
        "statement": {
            "agora_governance_key_cert": statement["agora_governance_key_cert"],
            "key": _hex_field(statement, "key", 32, where),
            "purpose": statement["purpose"],
            "from_seq": statement["from_seq"],
            "prev_hash": prev,
            "last_trusted": head,
        },
        "signatures": signatures,
    }


def parse_rotation(data):
    """A `key_rotation` entry's `data`, or a reason it is not one"""
    try:
        _exactly(
            data,
            _ROTATION_FIELDS - {"outgoing_certificate"},
            {"outgoing_certificate"},
            "key_rotation `data`",
        )
        if not _uint(data["agora_governance_key_rotation"], _U32):
            raise InputError("agora_governance_key_rotation")
        if data["reason"] not in ("routine", "compromise"):
            raise InputError("unknown reason %r" % (data["reason"],))
        signed_at = data["proof_signed_at"]
        if (
            not isinstance(signed_at, int)
            or isinstance(signed_at, bool)
            or not -(2**63) <= signed_at < 2**63
        ):
            raise InputError("proof_signed_at")
        outgoing = data.get("outgoing_certificate")
        rotation = {
            "version": data["agora_governance_key_rotation"],
            "reason": data["reason"],
            "old_key": _hex_field(data, "old_key", 32, "key_rotation"),
            "new_key": _hex_field(data, "new_key", 32, "key_rotation"),
            "proof": _hex_field(data, "proof", 64, "key_rotation"),
            "proof_signed_at": signed_at,
            "certificate": _parse_certificate(data["certificate"], "certificate"),
            "outgoing_certificate": None
            if outgoing is None
            else _parse_certificate(outgoing, "outgoing_certificate"),
        }
    except InputError as e:
        return None, "key_rotation `data` is malformed: %s" % e
    return rotation, None


def rotation_statement_hash(rotation, prev_hash):
    """What the proof of possession signs: this rotation, at this position"""
    fields = [
        ("agora_governance_key_rotation", KEY_ROTATION_VERSION),
        ("reason", rotation["reason"]),
        ("old_key", rotation["old_key"]),
        ("new_key", rotation["new_key"]),
        ("prev_hash", prev_hash),
    ]
    parts = []
    for name, value in fields:
        parts.append(json.dumps(name, ensure_ascii=False))
        parts.append(":")
        _write_canonical(value, parts)
        parts.append(",")
    return sha256_hex(("{" + "".join(parts[:-1]) + "}").encode("utf-8"))


def verify_proof(rotation, prev_hash):
    """Version and the proof of possession"""
    if rotation["version"] != KEY_ROTATION_VERSION:
        return "agora_governance_key_rotation is %r, not %d" % (
            rotation["version"],
            KEY_ROTATION_VERSION,
        )
    new_key = bytes.fromhex(rotation["new_key"])
    if _decompress(new_key) is None:
        return "new_key is not a valid Ed25519 public key"
    statement = bytes.fromhex(rotation_statement_hash(rotation, prev_hash))
    if not ed25519_verify(
        new_key,
        signed_message(statement, rotation["proof_signed_at"]),
        bytes.fromhex(rotation["proof"]),
    ):
        return (
            "the proof of possession does not verify for this rotation at "
            "this position"
        )
    return None


def root_signed_bytes(statement):
    """The exact bytes a root key signs"""
    return ROOT_DOMAIN + canonical_json(statement)


def verify_certificate(certificate, expected, roots, threshold):
    """`certificate` is the root's for exactly the statement `expected`,
    or the reason it is not"""
    statement = certificate["statement"]
    if statement["agora_governance_key_cert"] != KEY_CERT_VERSION:
        return "agora_governance_key_cert is %r, not %d" % (
            statement["agora_governance_key_cert"],
            KEY_CERT_VERSION,
        )
    if statement != expected:
        return "the certificate is for a different key, purpose or chain position"
    message = root_signed_bytes(statement)
    signers = set()
    for signature in certificate["signatures"]:
        if signature["root_key"] not in roots:
            continue
        if ed25519_verify(
            bytes.fromhex(signature["root_key"]),
            message,
            bytes.fromhex(signature["signature"]),
        ):
            signers.add(signature["root_key"])
    if len(signers) < threshold:
        return (
            "%d valid root signature(s) where %d are needed; unknown and "
            "repeated signers count for nothing" % (len(signers), threshold)
        )
    return None


def _statement(key, purpose, from_seq, prev_hash, last_trusted):
    return {
        "agora_governance_key_cert": KEY_CERT_VERSION,
        "key": key,
        "purpose": purpose,
        "from_seq": from_seq,
        "prev_hash": prev_hash,
        "last_trusted": last_trusted,
    }


def verify_certified(rotation, seq, prev_hash, roots, threshold):
    """The proof, and the certificate for this key at this position —
    everything checkable about a rotation without the rest of the chain"""
    problem = verify_proof(rotation, prev_hash)
    if problem:
        return problem
    if rotation["reason"] == "routine":
        expected = _statement(rotation["new_key"], "routine", seq + 1, prev_hash, None)
    else:
        # Only the root can say where the window opens, so this one field
        # is taken from the certificate; the signature covers it.
        head = rotation["certificate"]["statement"]["last_trusted"]
        if head is None:
            return "a compromise certificate must name last_trusted"
        expected = _statement(rotation["new_key"], "compromise", seq, prev_hash, head)
    problem = verify_certificate(rotation["certificate"], expected, roots, threshold)
    return "certificate: %s" % problem if problem else None


class KeyWalk:
    """The signing key history, as walking the chain discovers it"""

    def __init__(self, genesis_key, anchor, roots, threshold):
        self.genesis = genesis_key
        self.genesis_certified = False
        self.roots = roots
        self.threshold = max(1, threshold)
        self.history = [
            {
                "public_key": genesis_key,
                "from_seq": 1,
                "through_seq": None,
                "status": "active",
                "introduced_by": None,
                "retired_by": None,
                "certified": False,
            }
        ]
        self.unanchored = [] if genesis_key in anchor else [genesis_key]
        # Every key that has ever held the chain, voided ones included: not
        # even a root certificate brings one back.
        self.seen = {genesis_key}
        #: positions inside a compromise window
        self.repudiated = set()

    def in_force(self, seq):
        """The key that signs entry `seq`"""
        for record in reversed(self.history):
            if record["from_seq"] <= seq:
                return record["public_key"]
        return self.history[0]["public_key"]

    def active(self):
        """The key that signs whatever comes next"""
        return self.history[-1]["public_key"]

    def _close(self, through_seq, status, by):
        self.history[-1].update(
            through_seq=through_seq, status=status, retired_by=by
        )

    def _open(self, public_key, from_seq, by):
        self.history.append(
            {
                "public_key": public_key,
                "from_seq": from_seq,
                "through_seq": None,
                "status": "active",
                "introduced_by": by,
                "retired_by": None,
                "certified": True,
            }
        )

    def declares_compromise(self, rotation, seq, prev_hash):
        """(`True`, None) if `rotation` is a compromise declaration this
        verifier will check under the new key rather than the key in force
        — which takes the root's certificate for that key, here. Otherwise
        (`False`, why not), the reason being None for anything that is not
        a compromise declaration at all."""
        if rotation is None or rotation["reason"] != "compromise":
            return False, None
        if rotation["new_key"] in self.seen:
            return False, (
                "new_key has already held this chain; a key is never brought back"
            )
        problem = verify_certified(
            rotation, seq, prev_hash, self.roots, self.threshold
        )
        return problem is None, problem

    def apply(self, rotation, link, seq, links):
        """Follow `rotation`, appended as `link` at position `seq`"""
        problem = verify_certified(
            rotation,
            seq,
            link["attestation"]["prev_hash"],
            self.roots,
            self.threshold,
        )
        if problem:
            return problem
        if rotation["new_key"] in self.seen:
            return "new_key has already held this chain; a key is never brought back"
        # The genesis key predates the root, so the first rotation brings
        # its certificate along. Whether that rotation is later voided by a
        # compromise does not matter: the certificate is the root's
        # statement, not the entry's.
        outgoing = rotation["outgoing_certificate"]
        if outgoing is None and not self.genesis_certified:
            return (
                "the chain's first rotation must carry the genesis key's "
                "outgoing_certificate"
            )
        if outgoing is not None and self.genesis_certified:
            return (
                "outgoing_certificate belongs on the chain's first rotation "
                "and nowhere else"
            )
        if outgoing is not None:
            problem = verify_certificate(
                outgoing,
                _statement(self.genesis, "genesis", 1, None, None),
                self.roots,
                self.threshold,
            )
            if problem:
                return "outgoing_certificate: %s" % problem

        if rotation["reason"] == "routine":
            if rotation["old_key"] != self.in_force(seq):
                return "old_key is not the key that was in force"
            self._close(seq, "retired", link["id"])
            self._open(rotation["new_key"], seq + 1, link["id"])
        else:
            head = rotation["certificate"]["statement"]["last_trusted"]
            trusted = head["chain_seq"]
            if not (
                1 <= trusted < seq
                and links[trusted - 1]["id"] == head["id"]
                and links[trusted - 1]["attestation"]["entry_hash"]
                == head["entry_hash"]
            ):
                return "last_trusted does not name an earlier entry of this chain"
            # Trust cannot be anchored inside a window nobody trusts: a
            # reattestation restores the entry, not the ability to name it
            # here.
            if trusted in self.repudiated:
                return "last_trusted names an entry an earlier compromise repudiated"
            # The key in force at the last trusted entry: a rotation inside
            # the window is void with the rest of it.
            if rotation["old_key"] != self.in_force(trusted):
                return "old_key is not the key that was in force"
            self.history = [r for r in self.history if r["from_seq"] <= trusted]
            self._close(trusted, "compromised", link["id"])
            self._open(rotation["new_key"], seq, link["id"])
            self.repudiated.update(range(trusted + 1, seq))
        self.seen.add(rotation["new_key"])
        if not self.genesis_certified:
            self.genesis_certified = True
            self.history[0]["certified"] = True
            self.unanchored = []
        return None


# ---------------------------------------------------------------------------
# END key rotation, v2 (root certificates)
# ---------------------------------------------------------------------------


def _prefix_problem(link):
    """The id series an entry type must use, and must not"""
    prefix = link["id"][:3]
    expected = {"amendment": "AMD", "key_rotation": "KEY"}.get(link["entry_type"])
    if expected is not None and prefix != expected:
        return "the id of a %s entry must be in the %s- series, not %s" % (
            link["entry_type"],
            expected,
            link["id"],
        )
    if expected is None and prefix in ("AMD", "KEY"):
        return (
            "%s- ids are reserved for amendment and key_rotation entries, "
            "but %s is a %s" % (prefix, link["id"], link["entry_type"])
        )
    return None


def verify_chain(raw_links, genesis_key, anchor, roots=None, threshold=None):
    """Verify a whole chain from `genesis_key`, following the rotations the
    root keys certified and no others.

    `genesis_key` is the key the chain started under. It is not in the
    chain, so a verifier has to be told. Until the chain's first rotation
    certifies it, `anchor` is what vouches for it; if it does not, that is
    reported rather than fatal, since a client that pinned what it first
    saw is entitled to a clean report.

    `roots` and `threshold` default to ROOT_KEYS and ROOT_THRESHOLD; only
    the test vectors, which cannot sign with hardware keys, pass others.
    """
    genesis_key = genesis_key.lower()
    anchor = {key.lower() for key in anchor}
    roots = {key.lower() for key in (ROOT_KEYS if roots is None else roots)}
    threshold = ROOT_THRESHOLD if threshold is None else threshold
    links = sorted(
        (parse_link(raw) for raw in raw_links),
        key=lambda l: l["attestation"]["chain_seq"],
    )

    seen_ids = set()
    duplicates = set()
    for link in links:
        (duplicates if link["id"] in seen_ids else seen_ids).add(link["id"])
    position = {link["id"]: i + 1 for i, link in enumerate(links)}

    walk = KeyWalk(genesis_key, anchor, roots, threshold)
    amendments = []  # (seq, id, amendment, target seq), in chain order
    entries = []

    for index, link in enumerate(links):
        seq = index + 1
        a = link["attestation"]
        previous = links[index - 1] if index else None
        problems = []

        problem = _prefix_problem(link)
        if problem:
            problems.append(problem)
        if link["id"] in duplicates:
            problems.append("%s appears more than once" % link["id"])

        # Amendments and rotations are the entries a verifier has to read.
        # Their `data` is hashed against the envelope before it is parsed,
        # so what is read is what was signed.
        carries_meaning = link["entry_type"] in ("amendment", "key_rotation")
        amendment = rotation = None
        content_matches = None
        if link["data"] is not None:
            content_matches = data_hash(link["data"]) == a["data_hash"]
            if not content_matches:
                if carries_meaning:
                    problems.append("`data` does not hash to the attested data_hash")
            elif link["entry_type"] == "amendment":
                amendment, problem = parse_amendment(link["data"])
                if amendment is not None:
                    problem = validate_amendment(amendment)
                    if problem:
                        amendment = None
                if problem:
                    problems.append(problem)
            elif link["entry_type"] == "key_rotation":
                rotation, problem = parse_rotation(link["data"])
                if problem:
                    problems.append(problem)
        elif carries_meaning:
            problems.append("a %s entry must carry its `data`" % link["entry_type"])

        # A compromise declaration is signed by the key it moves to, and is
        # taken at its word only if the root certified that key here.
        # Everything else is signed by the key in force here.
        declared, why_not = walk.declares_compromise(rotation, seq, a["prev_hash"])
        if declared:
            signed_by = rotation["new_key"]
        else:
            signed_by = walk.in_force(seq)
            if why_not:
                # Say why the declaration was not taken at its word; the
                # bad signature that follows is the consequence.
                problems.append(why_not)

        created_micros, _ = parse_timestamp(link["created_at"])
        _, signed_seconds = parse_timestamp(a["signed_at"])
        hash_ok = True
        signature_valid = False
        if a["envelope_version"] != ENVELOPE_VERSION:
            hash_ok = False
            problems.append(
                "envelope version %r is not supported (this verifier knows %d)"
                % (a["envelope_version"], ENVELOPE_VERSION)
            )
        elif entry_hash(link) != a["entry_hash"]:
            hash_ok = False
            problems.append("entry_hash does not recompute from the envelope fields")
        elif ed25519_verify(
            bytes.fromhex(signed_by),
            signed_message(bytes.fromhex(a["entry_hash"]), signed_seconds),
            bytes.fromhex(a["signature"]),
        ):
            signature_valid = True
        else:
            problems.append("signature does not verify under the published key")

        link_valid = hash_ok
        if a["chain_seq"] != seq:
            link_valid = False
            problems.append(
                "chain_seq %d where %d was expected" % (a["chain_seq"], seq)
            )
        expected_prev = previous["attestation"]["entry_hash"] if previous else None
        if a["prev_hash"] != expected_prev:
            link_valid = False
            problems.append(
                "first entry names a predecessor"
                if expected_prev is None
                else "prev_hash is null mid-chain"
                if a["prev_hash"] is None
                else "prev_hash is not the previous entry's entry_hash"
            )

        # Only an entry that is itself authentic gets to move the key or
        # amend anything: a forged one already fails the chain and must not
        # also get to describe it.
        authentic = signature_valid and link_valid
        if rotation is not None and authentic:
            problem = walk.apply(rotation, link, seq, links)
            if problem and problem not in problems:
                problems.append(problem)
        # Texts are checked against what the entry signed whether or not
        # the amendment goes on to take effect: a substituted text is a lie
        # about the record either way. Beside anything that commits to no
        # texts, they are words nobody signed.
        texts = None
        if amendment is not None and authentic:
            texts, problem = text_status(amendment, link["texts"])
            if problem:
                problems.append(problem)
        elif link["texts"] and any(t is not None for t in link["texts"].values()):
            problems.append("texts beside an entry that commits to none")
        if amendment is not None and authentic:
            target = position.get(amendment["target"])
            if target is None:
                problems.append(
                    "amendment target %s is not an entry of this chain"
                    % amendment["target"]
                )
            elif target >= seq:
                problems.append(
                    "amendment target %s is not an earlier entry" % amendment["target"]
                )
            elif (
                links[target - 1]["attestation"]["entry_hash"]
                != amendment["target_entry_hash"]
            ):
                problems.append(
                    "target_entry_hash is not %s's entry_hash" % amendment["target"]
                )
            else:
                amendments.append((seq, link["id"], amendment, target))

        entries.append(
            {
                "id": link["id"],
                "chain_seq": a["chain_seq"],
                "entry_hash": a["entry_hash"],
                "attested_data_hash": a["data_hash"],
                "signature_valid": signature_valid,
                "link_valid": link_valid,
                "content_matches": content_matches,
                "retroactive": is_retroactive(created_micros, signed_seconds),
                "out_of_order": previous is not None
                and created_micros < parse_timestamp(previous["created_at"])[0],
                "amended_by": [],
                "signed_by": signed_by,
                "redacted": False,
                "redacted_data_hash": None,
                "repudiated": False,
                "reattested_by": [],
                "standing": "in_force",
                "texts": texts,
                "problem": "; ".join(problems) if problems else None,
            }
        )

    # Reattestations first, and to a fixpoint: an amendment inside a
    # repudiated window has no effect unless something later vouches for
    # the entry it sits in, and that something can be another
    # reattestation.
    pending = [a for a in amendments if a[2]["kind"] == "reattested"]
    while True:
        restored = False
        for seq, amendment_id, amendment, target in list(pending):
            if seq in walk.repudiated:
                continue
            pending.remove((seq, amendment_id, amendment, target))
            entries[target - 1]["reattested_by"].append(amendment_id)
            walk.repudiated.discard(target)
            restored = True
        if not restored:
            break

    for seq, amendment_id, amendment, target in amendments:
        if seq in walk.repudiated:
            continue
        entry = entries[target - 1]
        entry["amended_by"].append(amendment_id)
        entry["standing"] = KIND_STANDING.get(amendment["kind"], entry["standing"])
        if amendment["kind"] == "redaction":
            entry["redacted"] = True
            entry["redacted_data_hash"] = amendment["resulting_data_hash"]

    # A redacted entry's content is what the redaction left behind.
    for index, link in enumerate(links):
        entry = entries[index]
        if (
            entry["content_matches"] is False
            and link["data"] is not None
            and entry["redacted_data_hash"] == data_hash(link["data"])
        ):
            entry["content_matches"] = True

    repudiated = []
    for index, entry in enumerate(entries):
        if index + 1 in walk.repudiated:
            entry["repudiated"] = True
            repudiated.append(entry["id"])

    unanchored = []
    for key in walk.unanchored:
        if key not in unanchored:
            unanchored.append(key)

    report = {
        "public_key": walk.active(),
        "ok": False,
        "head": links[-1]["id"] if links else None,
        "entries": entries,
        "keys": walk.history,
        "unanchored_keys": unanchored,
        "repudiated": repudiated,
    }
    return settle(report)


def settle(report):
    """Recompute `ok` from the entries.

    Repudiated entries do not clear it by themselves: repudiation is a
    declared state, not a defect.
    """
    report["ok"] = all(
        entry["signature_valid"]
        and entry["link_valid"]
        and entry["content_matches"] is not False
        and entry["problem"] is None
        for entry in report["entries"]
    )
    return report


def check_content(report, entry_id, data):
    """Record whether `data` is the content the entry attested — or what a
    redaction of it lawfully left behind"""
    digest = data_hash(data)
    for entry in report["entries"]:
        if entry["id"] != entry_id:
            continue
        entry["content_matches"] = digest in (
            entry["attested_data_hash"],
            entry["redacted_data_hash"],
        )
        return entry["content_matches"]
    return False


# ---------------------------------------------------------------------------
# Fetching
# ---------------------------------------------------------------------------


def fetch_json(url, timeout=60):
    request = urllib.request.Request(
        url, headers={"User-Agent": "agora-governance-verifier/1"}
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def _endpoint(base, path):
    return base.rstrip("/") + "/" + path.lstrip("/")


def fetch_chain(base):
    """The chain, the genesis key, and how we came by it"""
    served = fetch_json(_endpoint(base, "api/governance/signing-key"))
    keys = None
    try:
        keys = fetch_json(_endpoint(base, "api/governance/signing-keys"))["keys"]
    except urllib.error.HTTPError as e:
        if e.code != 404:
            raise
    if keys:
        oldest = min(keys, key=lambda k: k["from_seq"])
        genesis, source = oldest["public_key"], "the signing-keys history"
    else:
        genesis, source = served["public_key"], "the served signing key"
    links = fetch_json(_endpoint(base, "api/governance/log/chain"))
    return links, genesis, served, keys, source


def fetch_contents(base, report):
    """Each entry's full `data`, for the content check. Returns what was
    read and the ids that could not be."""
    contents, missing = {}, []
    for entry in report["entries"]:
        url = _endpoint(base, "api/content/%s?detail=full" % entry["id"])
        try:
            body = fetch_json(url)
        except urllib.error.HTTPError:
            missing.append(entry["id"])
            continue
        if isinstance(body, dict) and body.get("data") is not None:
            contents[entry["id"]] = body["data"]
        else:
            missing.append(entry["id"])
    return contents, missing


# ---------------------------------------------------------------------------
# Trust on first use
# ---------------------------------------------------------------------------


def check_pin(path, report, genesis_key):
    """Compare this run against the last one, and remember this one.

    Returns the alarms — history that changed under us, which no single
    run can detect.
    """
    alarms = []
    pinned = None
    if os.path.exists(path):
        with open(path, "r", encoding="utf-8") as handle:
            pinned = json.load(handle)
    if pinned:
        if pinned.get("genesis_key") != genesis_key:
            alarms.append(
                "the genesis key changed since the last run: pinned %s, now %s"
                % (pinned.get("genesis_key"), genesis_key)
            )
        head = pinned.get("head") or {}
        seq = head.get("chain_seq")
        entries = report["entries"]
        if seq is not None:
            if seq > len(entries):
                alarms.append(
                    "the chain is shorter than it was: pinned head at seq %d, "
                    "now %d entries" % (seq, len(entries))
                )
            else:
                now = entries[seq - 1]
                if now["id"] != head.get("id"):
                    alarms.append(
                        "seq %d was %s and is now %s"
                        % (seq, head.get("id"), now["id"])
                    )
                elif now.get("entry_hash") != head.get("entry_hash"):
                    alarms.append(
                        "%s has a different entry_hash than when it was pinned"
                        % now["id"]
                    )
    if report["ok"] and not alarms and report["entries"]:
        last = report["entries"][-1]
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(
                {
                    "genesis_key": genesis_key,
                    "head": {
                        "chain_seq": last["chain_seq"],
                        "id": last["id"],
                        "entry_hash": last["entry_hash"],
                    },
                },
                handle,
                indent=2,
            )
            handle.write("\n")
    return alarms


# ---------------------------------------------------------------------------
# The report
# ---------------------------------------------------------------------------


def render(report, genesis_key, anchor, source, alarms, missing):
    lines = []
    entries = report["entries"]
    lines.append(
        "%d entries, head %s" % (len(entries), report["head"] or "(none)")
    )
    lines.append(
        "genesis key %s (%s)%s"
        % (genesis_key, source, "" if genesis_key in anchor else " — NOT ANCHORED")
    )
    lines.append("key in force %s" % report["public_key"])
    counts = {
        "retroactive": sum(1 for e in entries if e["retroactive"]),
        "out of order": sum(1 for e in entries if e["out_of_order"]),
        "amended": sum(1 for e in entries if e["amended_by"]),
        "redacted": sum(1 for e in entries if e["redacted"]),
        "content checked": sum(
            1 for e in entries if e["content_matches"] is not None
        ),
    }
    lines.append(
        ", ".join("%d %s" % (n, name) for name, n in counts.items() if n)
        or "no flags"
    )
    for entry in entries:
        if entry["problem"]:
            lines.append("  %s (seq %d): %s" % (entry["id"], entry["chain_seq"], entry["problem"]))
        elif entry["content_matches"] is False:
            lines.append(
                "  %s (seq %d): content does not hash to what was attested"
                % (entry["id"], entry["chain_seq"])
            )
    if report["repudiated"]:
        lines.append(
            "repudiated (signed inside a compromise window): %s"
            % ", ".join(report["repudiated"])
        )
    for record in report["keys"]:
        lines.append(
            "  key %s seq %s-%s %s%s"
            % (
                record["public_key"],
                record["from_seq"],
                record["through_seq"] or "",
                record["status"],
                ", root-certified" if record["certified"] else "",
            )
        )
    for key in report["unanchored_keys"]:
        lines.append(
            "UNANCHORED GENESIS KEY %s — neither this script's "
            "PUBLISHED_KEYS nor a root certificate in the chain vouches for "
            "the key the chain started under. Check it against "
            "agora-agentkit's PUBLISHED_KEYS and against what other agents "
            "report." % key
        )
    for alarm in alarms:
        lines.append("PIN ALARM: %s" % alarm)
    if missing:
        lines.append(
            "content unavailable for %d entries: %s"
            % (len(missing), ", ".join(missing))
        )
    verdict = report["ok"] and not alarms
    lines.append("VERDICT: %s" % ("ok" if verdict else "NOT OK"))
    if verdict:
        lines.append(
            "This proves the log is internally consistent, signed by the "
            "keys above, and that every change of key was certified by a "
            "root key in this script's ROOT_KEYS. It does not prove those "
            "are the Steward's — compare them with agora-agentkit's "
            "PUBLISHED_KEYS and ROOT_KEYS and with what other agents report."
        )
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        epilog="Anchor keys default to this script's PUBLISHED_KEYS.",
    )
    parser.add_argument(
        "--url", help="platform base URL, e.g. https://subliminal.technology/agora"
    )
    parser.add_argument("--chain", help="a file holding the chain JSON (offline)")
    parser.add_argument("--genesis-key", help="the key the chain started under (offline)")
    parser.add_argument(
        "--contents", help="a file mapping entry id to that entry's full `data`"
    )
    parser.add_argument(
        "--anchor",
        action="append",
        default=[],
        metavar="HEX",
        help="an out-of-band trusted key, repeatable",
    )
    parser.add_argument(
        "--check-content",
        action="store_true",
        help="with --url, also read every entry in full and hash it",
    )
    parser.add_argument("--pin", metavar="FILE", help="TOFU-pin the key and the head")
    parser.add_argument("--json", action="store_true", help="print the full verdict")
    args = parser.parse_args(argv)

    try:
        if args.chain:
            if not args.genesis_key:
                parser.error("--chain needs --genesis-key")
            with open(args.chain, "r", encoding="utf-8") as handle:
                links = json.load(handle)
            genesis, served, keys, source = args.genesis_key, None, None, "--genesis-key"
        elif args.url:
            links, genesis, served, keys, source = fetch_chain(args.url)
        else:
            parser.error("one of --url or --chain is required")

        anchor = [key.lower() for key in PUBLISHED_KEYS + args.anchor]
        report = verify_chain(links, genesis, anchor)
        contents, missing = {}, []
        if args.contents:
            with open(args.contents, "r", encoding="utf-8") as handle:
                contents = json.load(handle)
        elif args.check_content and args.url:
            contents, missing = fetch_contents(args.url, report)
        for entry_id, data in contents.items():
            check_content(report, entry_id, data)
        if contents:
            settle(report)

        alarms = check_pin(args.pin, report, genesis.lower()) if args.pin else []
    except InputError as e:
        print("input error: %s" % e, file=sys.stderr)
        return 2
    except (urllib.error.URLError, OSError, ValueError, KeyError) as e:
        print("%s: %s" % (type(e).__name__, e), file=sys.stderr)
        return 2

    derived = [k["public_key"] for k in report["keys"]]
    if served is not None and served.get("public_key", "").lower() not in derived:
        print(
            "WARNING: the platform serves a signing key the chain never "
            "names: %s" % served.get("public_key"),
            file=sys.stderr,
        )
    if keys is not None and [k["public_key"].lower() for k in keys] != derived:
        print(
            "WARNING: the platform's signing-key history is not the one the "
            "chain declares",
            file=sys.stderr,
        )
    print(render(report, genesis.lower(), anchor, source, alarms, missing))
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if report["ok"] and not alarms else 1


if __name__ == "__main__":
    sys.exit(main())
