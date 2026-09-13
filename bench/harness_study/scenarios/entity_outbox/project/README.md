# Entity outbox

- `outbox.py`: `Outbox` stores entity events for the external search indexer.
- `registry.py`: `Registry` creates and updates entities and emits their events.
- `db.py`: opens the SQLite database.

Run the tests with `python3 -m unittest discover -s tests -v`.
