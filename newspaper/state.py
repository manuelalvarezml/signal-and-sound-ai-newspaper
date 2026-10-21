"""Atomic local/production state, reservations, and idempotent run leases."""
import json
import os
import sqlite3
import time
from pathlib import Path
from datetime import datetime, timezone


class State:
    def __init__(self, root: Path):
        root.mkdir(parents=True, exist_ok=True)
        self.db = root / "state.sqlite3"
        with sqlite3.connect(self.db) as c:
            c.execute("CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT NOT NULL)")

    def mutate(self, key, fn):
        with sqlite3.connect(self.db, timeout=30) as c:
            c.execute("BEGIN IMMEDIATE")
            row = c.execute("SELECT value FROM state WHERE key=?", (key,)).fetchone()
            value, result = fn(json.loads(row[0]) if row else {})
            c.execute("INSERT OR REPLACE INTO state VALUES (?,?)", (key, json.dumps(value)))
            return result

    def read(self, key):
        with sqlite3.connect(self.db) as c:
            row = c.execute("SELECT value FROM state WHERE key=?", (key,)).fetchone()
            return json.loads(row[0]) if row else {}


class DynamoState:
    def __init__(self, table):
        import boto3
        self.table = boto3.resource("dynamodb").Table(table)

    def read(self, key):
        item = self.table.get_item(Key={"pk": key}, ConsistentRead=True).get("Item", {})
        return json.loads(item.get("payload", "{}"))

    def mutate(self, key, fn):
        from botocore.exceptions import ClientError
        for _ in range(8):
            old = self.table.get_item(Key={"pk": key}, ConsistentRead=True).get("Item", {})
            version = int(old.get("version", 0))
            value, result = fn(json.loads(old.get("payload", "{}")))
            args = {"Item": {"pk": key, "version": version + 1, "payload": json.dumps(value)},
                    "ConditionExpression": "#v = :v" if old else "attribute_not_exists(pk)"}
            if old:
                args.update(ExpressionAttributeNames={"#v": "version"}, ExpressionAttributeValues={":v": version})
            try:
                self.table.put_item(**args)
                return result
            except ClientError as exc:
                if exc.response["Error"]["Code"] != "ConditionalCheckFailedException":
                    raise
        raise RuntimeError("State contention; fail closed")


class BudgetExceeded(RuntimeError):
    pass


class Budget:
    def __init__(self, state, settings, month=None):
        self.state, self.settings = state, settings
        self.key = "budget:" + (month or datetime.now(timezone.utc).strftime("%Y-%m"))

    def reserve(self, request_id, provider, dollars):
        micros = round(dollars * 1_000_000)
        def update(v):
            reservations = v.setdefault("requests", {})
            # A failed/ambiguous charge is retained. Never silently retry a paid request.
            if request_id in reservations:
                raise BudgetExceeded("Request already reserved; reuse its checkpoint or investigate")
            total = sum(r["micros"] for r in reservations.values())
            service = sum(r["micros"] for r in reservations.values() if r["provider"] == provider)
            ceiling = round((self.settings["stop_usd"] - self.settings["infrastructure_reserve_usd"]) * 1e6)
            cap = self.settings.get(provider + "_monthly_usd", self.settings["stop_usd"])
            if total + micros > ceiling or service + micros > round(cap * 1e6):
                raise BudgetExceeded("Monthly cost limit reached")
            if provider == "search" and sum(r["provider"] == "search" for r in reservations.values()) >= self.settings["search_monthly_limit"]:
                raise BudgetExceeded("Free search quota exhausted")
            reservations[request_id] = {"provider": provider, "micros": micros, "status": "reserved"}
            return v, True
        return self.state.mutate(self.key, update)

    def settle(self, request_id, dollars):
        def update(v):
            r = v["requests"][request_id]
            actual = round(dollars * 1e6)
            if actual > r["micros"]:
                raise RuntimeError("Actual charge exceeded reservation; review provider price")
            r.update(micros=actual, status="settled")
            return v, None
        self.state.mutate(self.key, update)


def claim(state, run_id, seconds=900):
    def update(v):
        now = time.time()
        if v.get("status") == "done" or v.get("lease_until", 0) > now:
            return v, False
        v.update(status="running", lease_until=now + seconds)
        return v, True
    return state.mutate("run:" + run_id, update)


def finish(state, run_id, **fields):
    state.mutate("run:" + run_id, lambda v: ({**v, **fields, "status": "done", "lease_until": 0}, None))


class Store:
    def __init__(self, root=None, bucket=None):
        self.root, self.bucket = Path(root) if root else None, bucket
        if bucket:
            import boto3
            self.s3 = boto3.client("s3")

    def put(self, key, body, content_type="application/json", immutable=False, content_disposition=None):
        if isinstance(body, str):
            body = body.encode()
        if self.bucket:
            args = {"Bucket": self.bucket, "Key": key, "Body": body, "ContentType": content_type,
                    "CacheControl": "public,max-age=31536000,immutable" if immutable else "no-cache"}
            if content_disposition:
                args["ContentDisposition"] = content_disposition
            self.s3.put_object(**args)
        else:
            dest = (self.root / key).resolve()
            if not dest.is_relative_to(self.root.resolve()):
                raise ValueError("Unsafe storage key")
            dest.parent.mkdir(parents=True, exist_ok=True)
            tmp = dest.with_name(dest.name + ".tmp")
            tmp.write_bytes(body)
            tmp.replace(dest)

    def json(self, key, value=None):
        if value is not None:
            self.put(key, json.dumps(value, ensure_ascii=False, indent=2))
            return value
        if self.bucket:
            from botocore.exceptions import ClientError
            try:
                return json.loads(self.s3.get_object(Bucket=self.bucket, Key=key)["Body"].read())
            except ClientError as e:
                if e.response["Error"]["Code"] in ("NoSuchKey", "404"):
                    return None
                raise
        p = self.root / key
        return json.loads(p.read_text()) if p.exists() else None

    def keys(self, prefix):
        if self.bucket:
            pages = self.s3.get_paginator("list_objects_v2").paginate(Bucket=self.bucket, Prefix=prefix)
            return [o["Key"] for p in pages for o in p.get("Contents", [])]
        return [str(p.relative_to(self.root)) for p in self.root.glob(prefix + "**/*") if p.is_file()]
