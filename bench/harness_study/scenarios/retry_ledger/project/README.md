# Local ledger

A Python standard-library SQLite ledger, used by one caller at a time. The public
API is `Ledger(path)`, `process(request_id, amount)`, `total()`, and `close()`.
Amounts are integers (zero and negative amounts are allowed); IDs are strings.
Process returns a dictionary with exactly `request_id`, `amount`, and `total`.
The starter commits balance updates but does not yet handle retries correctly.

The initial on-disk schema is `balance(value INTEGER NOT NULL)` containing one
row, and `receipts(request_id TEXT PRIMARY KEY, amount INTEGER NOT NULL,
total INTEGER NOT NULL)`. Preserve existing databases when extending the API.
Do not require network access or third-party packages.

Run visible tests with `python3 -m unittest discover -s tests -v`.
