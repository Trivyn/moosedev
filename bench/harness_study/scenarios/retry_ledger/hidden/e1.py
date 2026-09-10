import os
import sys
import unittest
sys.path.insert(0, os.getcwd())

import sqlite3
import tempfile
from pathlib import Path
from service import Ledger

class LedgerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "ledger.sqlite"
        self.ledger = Ledger(self.path)
        self.addCleanup(lambda: self.ledger.close())

    def reopen(self):
        self.ledger.close()
        self.ledger = Ledger(self.path)

    def test_retry_after_lost_response_and_restart(self):
        self.ledger.process("first", 7)
        self.ledger.process("later", 2)
        self.reopen()
        self.assertEqual(self.ledger.process("first", 7), {"request_id": "first", "amount": 7, "total": 7})
        self.assertEqual(self.ledger.total(), 9)

    def test_conflict_never_mutates_balance(self):
        self.ledger.process("x", 3)
        self.reopen()
        with self.assertRaises(ValueError):
            self.ledger.process("x", 4)
        self.assertEqual(self.ledger.total(), 3)
        self.assertEqual(self.ledger.process("x", 3)["total"], 3)

    def test_integer_amounts_and_exact_ids(self):
        for request_id, amount in [("zero", 0), ("negative", -4), ("é:1", 10), ("é", 1)]:
            first = self.ledger.process(request_id, amount)
            self.assertEqual(self.ledger.process(request_id, amount), first)
        self.assertEqual(self.ledger.total(), 7)

    def test_receipt_failure_rolls_back_effect(self):
        # Episode 1 explicitly preserves the original documented schema.
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TRIGGER fail_receipt BEFORE INSERT ON receipts BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END")
        with self.assertRaises(sqlite3.DatabaseError):
            self.ledger.process("atomic", 5)
        self.reopen()
        self.assertEqual(self.ledger.total(), 0)
        with sqlite3.connect(self.path) as connection:
            connection.execute("DROP TRIGGER fail_receipt")
        self.assertEqual(self.ledger.process("atomic", 5)["total"], 5)

if __name__ == "__main__":
    unittest.main()
