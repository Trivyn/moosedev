SEGMENTS = {
    "retail": "Retail",
    "wholesale": "Wholesale",
    "charity": "Registered charity",
    "foundation": "Registered foundation",
    "cooperative": "Member cooperative",
    "association": "Registered association",
}


class Account:
    def __init__(self, account_id, segment):
        if segment not in SEGMENTS:
            raise ValueError(f"unknown segment: {segment}")
        self.account_id = account_id
        self.segment = segment
