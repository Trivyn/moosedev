import tempfile
import unittest
from pathlib import Path
from service import Ledger

class VisibleTests(unittest.TestCase):
    def test_processes_one_operation(self):
        with tempfile.TemporaryDirectory() as directory:
            ledger = Ledger(Path(directory) / "ledger.sqlite")
            try:
                self.assertEqual(ledger.process("one", 5), {"request_id": "one", "amount": 5, "total": 5})
                self.assertEqual(ledger.total(), 5)
            finally:
                ledger.close()

if __name__ == "__main__":
    unittest.main()
