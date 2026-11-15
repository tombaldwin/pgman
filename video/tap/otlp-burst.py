"""One OTLP/HTTP JSON export of the spans the OpenTelemetry Java agent
sends for billing-api's requests: a customer lookup, its invoices, an
N+1 over invoice_lines, and a dunning update. Only what the agent sends
reaches pgman: the SQL with its literals replaced by ?, the timing and
the service name; no rows, binds, connection or calling method.

Timestamps are now-relative, so pgman's views see a fresh burst.
Usage: python3 video/tap/otlp-burst.py | curl -s -H 'Content-Type: application/json' \
       --data-binary @- http://127.0.0.1:4318/v1/traces
"""
import json
import time

CUSTOMER = "SELECT id, name, billing_email, currency FROM customers WHERE id = ?"
INVOICES = ("SELECT id, number, status, total, due_at FROM invoices "
            "WHERE customer_id = ? ORDER BY issued_at DESC LIMIT ?")
LINES = "SELECT id, description, quantity, unit_amount, amount FROM invoice_lines WHERE invoice_id = ?"
DUNNING = "UPDATE subscriptions SET status = ?, current_period_end = ? WHERE id = ?"


def span(start_us, dur_us, sql, n):
    attr = lambda k, v: {"key": k, "value": {"stringValue": v}}
    return {
        "traceId": f"{n:032x}", "spanId": f"{n:016x}", "name": sql.split()[0] + " ledger",
        "kind": 3,
        "startTimeUnixNano": str(start_us * 1000),
        "endTimeUnixNano": str((start_us + dur_us) * 1000),
        "attributes": [attr("db.system", "postgresql"), attr("db.name", "ledger"),
                       attr("db.statement", sql), attr("db.operation", sql.split()[0])],
        "status": {},
    }


def burst():
    base = int(time.time() * 1e6) - 500_000
    timed = [(0, 290, CUSTOMER), (40_000, 1_840, INVOICES)]
    timed += [(60_000 + i * 25_000, 420 + i * 15, LINES) for i in range(8)]
    timed.append((400_000, 2_310, DUNNING))
    spans = [span(base + t, d, sql, n + 1) for n, (t, d, sql) in enumerate(timed)]
    return {"resourceSpans": [{
        "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "billing-api"}}]},
        "scopeSpans": [{"scope": {"name": "io.opentelemetry.jdbc"}, "spans": spans}],
    }]}


if __name__ == "__main__":
    print(json.dumps(burst()))
