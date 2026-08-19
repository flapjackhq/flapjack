#!/usr/bin/env python3
"""Read-only loopback Algolia provider for the canonical PBV2 catalog fixture."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


def canonical_digest(value: Any) -> str:
    return hashlib.sha256(
        json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()
    ).hexdigest()


def fixture_source_snapshot(fixture: dict[str, Any]) -> dict[str, Any]:
    replicas = fixture["oracles"]["replicas"]
    return {
        "documents": fixture["documents"],
        "settings": fixture["settings"],
        "synonyms": fixture["synonyms"],
        "rules": fixture["rules"],
        "source_primary": replicas["source_primary"],
        "source_query_replicas": replicas["source_query_replicas"],
    }


class ProviderState:
    def __init__(self, fixture: dict[str, Any], app_id: str, api_key: str) -> None:
        self.fixture = fixture
        self.app_id = app_id
        self.api_key = api_key
        self.snapshot = fixture_source_snapshot(fixture)
        self.source_digest = canonical_digest(self.snapshot)
        self.requests: list[dict[str, Any]] = []
        self.mutation_attempts = 0
        self.browse_delay_ms = 0
        self.browse_failures_remaining = 0
        self.browse_successes_before_failure = 0
        self.lock = threading.Lock()

    @property
    def primary(self) -> str:
        return self.snapshot["source_primary"]

    @property
    def replicas(self) -> list[str]:
        return self.snapshot["source_query_replicas"]

    def record(self, method: str, path: str, body: Any) -> None:
        with self.lock:
            self.requests.append({"method": method, "path": path, "body": body})

    def evidence(self) -> dict[str, Any]:
        with self.lock:
            return {
                "fixture_id": self.fixture["fixture_id"],
                "source_digest": self.source_digest,
                "mutation_attempts": self.mutation_attempts,
                "request_count": len(self.requests),
                "requests": list(self.requests),
                "browse_delay_ms": self.browse_delay_ms,
                "browse_failures_remaining": self.browse_failures_remaining,
                "browse_successes_before_failure": self.browse_successes_before_failure,
            }

    def configure(self, body: Any) -> dict[str, Any]:
        if not isinstance(body, dict):
            raise ValueError("control body must be an object")
        delay = body.get("browse_delay_ms", self.browse_delay_ms)
        if (
            not isinstance(delay, int)
            or isinstance(delay, bool)
            or not 0 <= delay <= 30_000
        ):
            raise ValueError("browse_delay_ms must be an integer from 0 through 30000")
        failures = body.get("browse_failures_remaining", self.browse_failures_remaining)
        if (
            not isinstance(failures, int)
            or isinstance(failures, bool)
            or not 0 <= failures <= 10
        ):
            raise ValueError("browse_failures_remaining must be an integer from 0 through 10")
        successes = body.get(
            "browse_successes_before_failure", self.browse_successes_before_failure
        )
        if (
            not isinstance(successes, int)
            or isinstance(successes, bool)
            or not 0 <= successes <= 10
        ):
            raise ValueError("browse_successes_before_failure must be an integer from 0 through 10")
        with self.lock:
            self.browse_delay_ms = delay
            self.browse_failures_remaining = failures
            self.browse_successes_before_failure = successes
        return {
            "browse_delay_ms": delay,
            "browse_failures_remaining": failures,
            "browse_successes_before_failure": successes,
        }

    def consume_browse_failure(self) -> bool:
        with self.lock:
            if self.browse_successes_before_failure > 0:
                self.browse_successes_before_failure -= 1
                return False
            if self.browse_failures_remaining == 0:
                return False
            self.browse_failures_remaining -= 1
            return True


def build_handler(state: ProviderState) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = "PBV2AlgoliaFixture/1"

        def _json_body(self) -> Any:
            length = int(self.headers.get("content-length", "0") or "0")
            if length == 0:
                return {}
            return json.loads(self.rfile.read(length))

        def _send(self, status: int, payload: Any) -> None:
            body = json.dumps(payload, separators=(",", ":")).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def _authorized(self) -> bool:
            return (
                self.headers.get("x-algolia-application-id") == state.app_id
                and self.headers.get("x-algolia-api-key") == state.api_key
            )

        def _route(self, method: str) -> None:
            parsed = urllib.parse.urlsplit(self.path)
            if parsed.path == "/__state" and method == "GET":
                self._send(200, state.evidence())
                return
            if parsed.path == "/__control" and method == "POST":
                try:
                    self._send(200, state.configure(self._json_body()))
                except (ValueError, UnicodeDecodeError) as error:
                    self._send(400, {"message": str(error)})
                return
            if not self._authorized():
                self._send(403, {"message": "invalid fixture credentials"})
                return
            try:
                body = self._json_body() if method == "POST" else {}
            except (ValueError, UnicodeDecodeError):
                self._send(400, {"message": "invalid JSON"})
                return
            state.record(method, self.path, body)

            if (
                method == "POST"
                and parsed.path.endswith("/browse")
                and state.consume_browse_failure()
            ):
                self._send(503, {"message": "fixture transient source interruption"})
                return

            if method not in {"GET", "POST"}:
                with state.lock:
                    state.mutation_attempts += 1
                self._send(405, {"message": "PBV2 fixture source is read-only"})
                return
            response = route_read(state, method, parsed, body)
            if response is None:
                if method == "POST" and not parsed.path.endswith(("/browse", "/query", "/rules/search", "/synonyms/search")):
                    with state.lock:
                        state.mutation_attempts += 1
                self._send(404, {"message": "fixture route not found"})
                return
            self._send(200, response)

        def do_GET(self) -> None:  # noqa: N802
            self._route("GET")

        def do_POST(self) -> None:  # noqa: N802
            self._route("POST")

        def do_PUT(self) -> None:  # noqa: N802
            self._route("PUT")

        def do_PATCH(self) -> None:  # noqa: N802
            self._route("PATCH")

        def do_DELETE(self) -> None:  # noqa: N802
            self._route("DELETE")

        def log_message(self, *_: Any) -> None:
            return

    return Handler


def route_read(
    state: ProviderState,
    method: str,
    parsed: urllib.parse.SplitResult,
    body: Any,
) -> Any | None:
    if method == "GET" and parsed.path == "/1/indexes":
        query = urllib.parse.parse_qs(parsed.query)
        page = int(query.get("page", ["0"])[0])
        if page != 0:
            return {"items": [], "page": page, "nbPages": 1}
        records = [
            {
                "name": state.primary,
                "entries": len(state.snapshot["documents"]),
                "dataSize": len(json.dumps(state.snapshot["documents"]).encode()),
                "fileSize": len(json.dumps(state.snapshot["documents"]).encode()),
                "updatedAt": "2026-08-18T00:00:00Z",
                "lastBuildTimeS": 1,
                "pendingTask": False,
                "primary": None,
                "replicas": state.replicas,
            }
        ]
        records.extend(
            {
                "name": replica,
                "entries": len(state.snapshot["documents"]),
                "dataSize": len(json.dumps(state.snapshot["documents"]).encode()),
                "fileSize": len(json.dumps(state.snapshot["documents"]).encode()),
                "updatedAt": "2026-08-18T00:00:00Z",
                "lastBuildTimeS": 1,
                "pendingTask": False,
                "primary": state.primary,
                "replicas": [],
            }
            for replica in state.replicas
        )
        return {"items": records, "page": 0, "nbPages": 1}

    match = re.fullmatch(r"/1/indexes/([^/]+)/(settings|browse|query|rules/search|synonyms/search)", parsed.path)
    if match is None:
        return None
    index_name = urllib.parse.unquote(match.group(1))
    action = match.group(2)
    if index_name not in [state.primary, *state.replicas]:
        return None
    if action == "settings" and method == "GET":
        if index_name == state.primary:
            return {
                **state.snapshot["settings"],
                "replicas": [f"virtual({name})" for name in state.replicas],
            }
        return {
            **state.snapshot["settings"],
            "ranking": [
                "asc(price)",
                "typo",
                "geo",
                "words",
                "filters",
                "proximity",
                "attribute",
                "exact",
                "custom",
            ],
            "customRanking": [],
            "relevancyStrictness": 0,
        }
    # FJCloud admission performs a credential-only GET probe before Flapjack's
    # actual paged POST browse. Keep that probe immediate so the control delay
    # affects only the engine export phase under cancellation/interruption proof.
    if action == "browse" and method == "GET":
        return {"hits": []}
    if action == "browse" and method == "POST":
        with state.lock:
            delay_ms = state.browse_delay_ms
        if delay_ms:
            time.sleep(delay_ms / 1000)
        return {"hits": state.snapshot["documents"]}
    if action == "rules/search" and method == "POST":
        return paged_hits(state.snapshot["rules"])
    if action == "synonyms/search" and method == "POST":
        return paged_hits(state.snapshot["synonyms"])
    if action == "query" and method == "POST":
        return source_search(state, index_name, body)
    return None


def paged_hits(hits: list[dict[str, Any]]) -> dict[str, Any]:
    return {"hits": hits, "nbHits": len(hits), "page": 0, "nbPages": 1}


def source_search(state: ProviderState, index_name: str, body: Any) -> dict[str, Any]:
    query = str(body.get("query", body.get("q", ""))).lower()
    documents = list(state.snapshot["documents"])
    if query:
        if query == state.fixture["oracles"]["search"]["typo_query"]:
            object_ids = state.fixture["oracles"]["search"]["typo_order"]
        else:
            terms = query.split()
            documents = [
                document
                for document in documents
                if all(
                    term in " ".join(str(document.get(field, "")).lower() for field in ("title", "description", "brand"))
                    for term in terms
                )
            ]
            object_ids = [document["objectID"] for document in documents]
    else:
        object_ids = [document["objectID"] for document in documents]
    by_id = {document["objectID"]: document for document in state.snapshot["documents"]}
    selected = [by_id[object_id] for object_id in object_ids if object_id in by_id]
    filters = str(body.get("filters", ""))
    if filters:
        key, _, value = filters.partition(":")
        selected = [document for document in selected if str(document.get(key)) == value]
    if index_name in state.replicas:
        selected.sort(key=lambda document: (int(document["price"]), document["objectID"]))
    else:
        selected.sort(key=lambda document: (-int(document["popularity"]), int(document["price"])))
    page = int(body.get("page", 0))
    hits_per_page = int(body.get("hitsPerPage", state.snapshot["settings"].get("hitsPerPage", 20)))
    start = page * hits_per_page
    page_hits = selected[start : start + hits_per_page]
    return {
        "hits": page_hits,
        "nbHits": len(selected),
        "page": page,
        "nbPages": (len(selected) + hits_per_page - 1) // hits_per_page if hits_per_page else 0,
        "hitsPerPage": hits_per_page,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--app-id", default="PBV2APP")
    parser.add_argument("--api-key", default="pbv2-loopback-source-key")
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    fixture = json.loads(args.fixture.read_text(encoding="utf-8"))
    state = ProviderState(fixture, args.app_id, args.api_key)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), build_handler(state))
    print(
        json.dumps(
            {
                "base_url": f"http://127.0.0.1:{server.server_address[1]}",
                "source_digest": state.source_digest,
            },
            separators=(",", ":"),
        ),
        flush=True,
    )
    server.serve_forever()


if __name__ == "__main__":
    main()
