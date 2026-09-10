"""Remove provisioned credentials from observable evidence before publication."""
import base64
import json


class CredentialFilter:
    def __init__(self):
        self.values = set()

    def register_json(self, data):
        def visit(value):
            if isinstance(value, dict):
                for key, item in value.items():
                    if any(word in key.lower() for word in ("token", "key", "secret", "password")) and isinstance(item, str) and len(item) >= 8:
                        self.values.add(item)
                    visit(item)
            elif isinstance(value, list):
                for item in value:
                    visit(item)
        visit(json.loads(data))

    def bytes(self, data):
        for value in sorted(self.values, key=len, reverse=True):
            data = data.replace(value.encode(), b"<credential-redacted>")
        return data

    def event(self, value):
        if isinstance(value, dict):
            result = {key: self.event(item) for key, item in value.items()}
            for key in ("raw_base64", "stdout_base64", "stderr_base64"):
                if key in result:
                    raw = base64.b64decode(result[key])
                    cleaned = self.bytes(raw)
                    result[key] = base64.b64encode(cleaned).decode()
                    if raw != cleaned:
                        result["credentials_redacted"] = True
            return result
        if isinstance(value, list):
            return [self.event(item) for item in value]
        if isinstance(value, str):
            return self.bytes(value.encode()).decode()
        return value
