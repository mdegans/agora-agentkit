#!/usr/bin/env python3
"""Run `vectors/govlog/*.json` through the Python verifier.

The same files are asserted by the Rust verifier in
`src/govlog/vectors.rs`. A failure here means the two implementations
disagree about a rule, which is worth more than either of them being
quietly right: read the vector before fixing the script.

    python3 tools/test_vectors.py        # or: just test-python
"""

import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import verify_governance_log as govlog  # noqa: E402

VECTORS = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "vectors",
    "govlog",
)


def run_vector(vector):
    """The verdict for one vector file"""
    report = govlog.verify_chain(
        vector["links"],
        vector["genesis_key"],
        vector["anchor"],
        vector["root_keys"],
        vector["root_threshold"],
    )
    contents = vector.get("contents") or {}
    for entry_id, data in contents.items():
        govlog.check_content(report, entry_id, data)
    if contents:
        govlog.settle(report)
    return report


def observed(report):
    """The verdict in the shape a vector's `expect` block records"""
    return {
        "ok": report["ok"],
        "head": report["head"],
        "public_key": report["public_key"],
        "repudiated": report["repudiated"],
        "unanchored_keys": report["unanchored_keys"],
        "keys": report["keys"],
        "entries": [
            {
                "id": e["id"],
                "signature_valid": e["signature_valid"],
                "link_valid": e["link_valid"],
                "content_matches": e["content_matches"],
                "retroactive": e["retroactive"],
                "out_of_order": e["out_of_order"],
                "redacted": e["redacted"],
                "repudiated": e["repudiated"],
                "amended_by": e["amended_by"],
                "problem": e["problem"] is not None,
            }
            for e in report["entries"]
        ],
        "standing": {
            e["id"]: e["standing"] for e in report["entries"] if e["amended_by"]
        },
    }


class Vectors(unittest.TestCase):
    """One test per vector file, named after it"""

    maxDiff = None


def _case(path):
    def test(self):
        with open(path, "r", encoding="utf-8") as handle:
            vector = json.load(handle)
        expect = dict(vector["expect"])
        expect.setdefault("standing", {})
        self.assertEqual(observed(run_vector(vector)), expect, vector["description"])

    return test


def _install():
    names = sorted(f for f in os.listdir(VECTORS) if f.endswith(".json"))
    if not names:
        raise SystemExit("no vectors in %s" % VECTORS)
    for name in names:
        setattr(Vectors, "test_" + name[:-5], _case(os.path.join(VECTORS, name)))


_install()


class PublishedKeys(unittest.TestCase):
    def test_every_published_key_is_a_curve_point(self):
        for key in govlog.PUBLISHED_KEYS:
            self.assertIsNotNone(govlog._decompress(bytes.fromhex(key)), key)


class CanonicalJson(unittest.TestCase):
    """The formatting `data_hash` depends on, pinned against serde_json's"""

    def test_keys_are_sorted_at_every_level(self):
        value = {"b": {"z": 1, "a": [{"y": 2, "x": 3}]}, "a": None}
        self.assertEqual(
            govlog.canonical_json(value), b'{"a":null,"b":{"a":[{"x":3,"y":2}],"z":1}}'
        )

    def test_compact_and_escaped_like_serde(self):
        value = {"s": 'tab\there "q" ünïcode \U0001F600', "n": [1, -2, 3.5, True, False]}
        self.assertEqual(
            govlog.canonical_json(value).decode("utf-8"),
            '{"n":[1,-2,3.5,true,false],"s":"tab\\there \\"q\\" '
            'ünïcode \U0001F600"}',
        )

    def test_control_characters(self):
        self.assertEqual(
            govlog.canonical_json("\x00\x08\x0b\x1f\n").decode("utf-8"),
            '"\\u0000\\b\\u000b\\u001f\\n"',
        )

    def test_empty_containers(self):
        self.assertEqual(govlog.canonical_json({"a": {}, "b": []}), b'{"a":{},"b":[]}')


class Envelope(unittest.TestCase):
    """The pinned envelope from `envelope_v1_preimage_and_hash_are_pinned`"""

    PREIMAGE = (
        b'{"agora_governance_log":1,"id":"GOV-2026-0006",'
        b'"entry_type":"council_decision","created_at":1700000000123456,'
        b'"prev_hash":"1111111111111111111111111111111111111111111111111111'
        b'111111111111","data_hash":"a4adf645ae3f60c56484d01aea87d6d490321d'
        b'7fc66b1607df14b023fe567c7b"}'
    )

    def test_preimage_and_hash(self):
        data_hash = govlog.data_hash({"outcome": "approved", "title": "Ratification"})
        self.assertEqual(
            data_hash, "a4adf645ae3f60c56484d01aea87d6d490321d7fc66b1607df14b023fe567c7b"
        )
        preimage = govlog.envelope_preimage(
            "GOV-2026-0006",
            "council_decision",
            1700000000123456,
            "11" * 32,
            data_hash,
        )
        self.assertEqual(preimage, self.PREIMAGE)
        self.assertEqual(
            govlog.sha256_hex(preimage),
            "ba27577432f81e415f1c01cc4cfabab6070e3ac50fd468fffe195ef19c0e9464",
        )

    def test_timestamps(self):
        self.assertEqual(
            govlog.parse_timestamp("2023-11-14T22:13:20.123456789Z"),
            (1700000000123456, 1700000000),
        )
        self.assertEqual(
            govlog.parse_timestamp("2023-11-14T22:13:20Z"),
            (1700000000000000, 1700000000),
        )
        self.assertEqual(
            govlog.parse_timestamp("2023-11-14T23:13:20+01:00")[1], 1700000000
        )
        self.assertEqual(
            govlog.parse_timestamp("2026-03-28T19:08:08.089439Z")[0],
            1774724888089439,
        )


class Ed25519(unittest.TestCase):
    """RFC 8032 §7.1 vector 2, and what strict verification refuses"""

    KEY = bytes.fromhex(
        "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"
    )
    MESSAGE = bytes.fromhex("72")
    SIGNATURE = bytes.fromhex(
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da"
        "085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"
    )

    def test_accepts_a_good_signature(self):
        self.assertTrue(govlog.ed25519_verify(self.KEY, self.MESSAGE, self.SIGNATURE))

    def test_rejects_a_changed_message(self):
        self.assertFalse(govlog.ed25519_verify(self.KEY, b"\x73", self.SIGNATURE))

    def test_rejects_a_non_canonical_s(self):
        forged = self.SIGNATURE[:32] + (
            int.from_bytes(self.SIGNATURE[32:], "little") + govlog._L
        ).to_bytes(32, "little")
        self.assertFalse(govlog.ed25519_verify(self.KEY, self.MESSAGE, forged))

    def test_rejects_the_identity_element_forgery(self):
        # The attack `crypto::verify` documents: with A the identity, the
        # cofactored equation degenerates to [s]B == R, so R = [1]B and
        # s = 1 verify against arbitrary messages. Strict verification
        # refuses the small-order key instead.
        basepoint = bytes.fromhex("58" + "66" * 31)
        signature = basepoint + (1).to_bytes(32, "little")
        for i in range(8):
            message = ("attack payload %d" % i).encode()
            self.assertFalse(govlog.ed25519_verify(b"\x00" * 32, message, signature))


if __name__ == "__main__":
    unittest.main(verbosity=2)
