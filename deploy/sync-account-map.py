#!/usr/bin/env python3
"""同步 CPR 数据库中的账户出口代理到桥接 account-map.json。

数据源：provider_accounts（enabled + outbound_proxy_url）。
产出：{"accounts": {"<account_id>": {"proxy": "...", "direct": false}}, "version": "<iso8601>"}
规则：
- 账户有代理 → 写 proxy；
- 账户无代理且启用 → 写 {"direct": true}（桥接语义为显式直连）；
- 账户停用 → 不写入（桥接会拒绝，符合失败关闭原则）。
原子替换写入；连接参数经环境变量传入，不落盘。
"""
import json
import os
import sys
import tempfile
from datetime import datetime, timezone

import psycopg2

OUTPUT = os.environ.get("EXCEL_BRIDGE_ACCOUNT_MAP", "/var/lib/cpr-excel-companion/account-map.json")
PG = {
    "host": os.environ.get("PGHOST", "127.0.0.1"),
    "port": os.environ.get("PGPORT", "5432"),
    "dbname": os.environ.get("PGDATABASE", "codex_proxy"),
    "user": os.environ.get("PGUSER", "codex_proxy"),
    "password": os.environ["PGPASSWORD"],
}


def main() -> int:
    connection = psycopg2.connect(**PG)
    try:
        cursor = connection.cursor()
        cursor.execute("select id, outbound_proxy_url, upstream_account_id from provider_accounts where enabled AND provider_kind = 'openai'")
        rows = cursor.fetchall()
    finally:
        connection.close()
    accounts = {}
    for account_id, proxy_url, upstream_account_id in rows:
        if proxy_url:
            accounts[account_id] = {"proxy": proxy_url, "direct": False}
        else:
            accounts[account_id] = {"direct": True}
        if upstream_account_id:
            accounts[account_id]["upstream_account_id"] = upstream_account_id
    payload = {
        "accounts": accounts,
        "version": datetime.now(timezone.utc).isoformat(timespec="seconds"),
    }
    directory = os.path.dirname(OUTPUT)
    os.makedirs(directory, exist_ok=True)
    handle, temporary = tempfile.mkstemp(dir=directory, prefix=".account-map-", suffix=".tmp")
    try:
        with os.fdopen(handle, "w") as file:
            json.dump(payload, file, indent=2)
            file.write("\n")
        os.chmod(temporary, 0o640)
        os.replace(temporary, OUTPUT)
    except BaseException:
        os.unlink(temporary)
        raise
    print(f"synced {len(accounts)} accounts -> {OUTPUT}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyError as missing:
        print(f"missing env {missing}", file=sys.stderr)
        sys.exit(2)
