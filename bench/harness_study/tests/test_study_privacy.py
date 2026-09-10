import base64
import json
import unittest

from bench.harness_study.privacy import CredentialFilter


class PrivacyTests(unittest.TestCase):
    def test_provisioned_credentials_cannot_survive_text_or_encoded_wire_evidence(self):
        scrub = CredentialFilter()
        token = "private-test-token-12345678"
        scrub.register_json(json.dumps({"tokens": {"access_token": token}}))
        raw = ("a response echoed " + token).encode()
        cleaned = scrub.event({"text": raw.decode(), "raw_base64": base64.b64encode(raw).decode(),
                               "nested": [{"description": token}]})
        self.assertNotIn(token, json.dumps(cleaned))
        self.assertNotIn(token.encode(), base64.b64decode(cleaned["raw_base64"]))
        self.assertTrue(cleaned["credentials_redacted"])

    def test_nonsecret_evidence_is_unchanged(self):
        scrub = CredentialFilter()
        value = {"text": "ordinary complete evidence", "tokens": 99, "raw_base64": "YWJj"}
        self.assertEqual(scrub.event(value), value)
