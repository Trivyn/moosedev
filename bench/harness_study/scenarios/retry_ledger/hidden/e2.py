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

    def test_accounts_partition_identity_and_balance(self):
        self.ledger.process("shared", 7, account="a")
        self.ledger.process("shared", 3, account="b")
        self.ledger.process("a:b", 2, account="c")
        self.ledger.process("b", 4, account="c:a")
        self.reopen()
        self.assertEqual(self.ledger.process("shared", 7, account="a")["total"], 7)
        self.assertEqual(self.ledger.total("a"), 7)
        self.assertEqual(self.ledger.total("b"), 3)
        self.assertEqual(self.ledger.total("c"), 2)
        self.assertEqual(self.ledger.total("c:a"), 4)
        self.assertEqual(self.ledger.total(), 0)
        with self.assertRaises(ValueError):
            self.ledger.process("shared", 99, account="a")
        self.assertEqual(self.ledger.total("b"), 3)

    def test_migrates_original_database_into_default_account(self):
        legacy = Path(self.directory.name) / "legacy.sqlite"
        with sqlite3.connect(legacy) as connection:
            connection.execute("CREATE TABLE balance (value INTEGER NOT NULL)")
            connection.execute("INSERT INTO balance VALUES (9)")
            connection.execute("CREATE TABLE receipts (request_id TEXT PRIMARY KEY, amount INTEGER NOT NULL, total INTEGER NOT NULL)")
            connection.executemany("INSERT INTO receipts VALUES (?, ?, ?)", [("one", 7, 7), ("two", 2, 9)])
        ledger = Ledger(legacy)
        try:
            self.assertEqual(ledger.process("one", 7)["total"], 7)
            self.assertEqual(ledger.total(), 9)
            self.assertEqual(ledger.process("one", 4, account="new")["total"], 4)
            self.assertEqual(ledger.total(), 9)
        finally:
            ledger.close()

if __name__ == "__main__":
    unittest.main()
