#!/usr/bin/env bash
# Focused contract tests for publication_repair_cli_live.sh runner validation.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$ENGINE_DIR/.." && pwd)"
RUNNER="$SCRIPT_DIR/publication_repair_cli_live.sh"
TMP_ROOT=""

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    rm -rf "$TMP_ROOT"
  fi
}
trap cleanup EXIT

make_workspace() {
  TMP_ROOT="$(mktemp -d /tmp/flapjack_publication_runner_test.XXXXXX)"
  TMP_ROOT="$(cd "$TMP_ROOT" && pwd -P)"
  mkdir -p "$TMP_ROOT/bin" "$TMP_ROOT/artifacts"
}

reset_artifacts() {
  rm -rf "$TMP_ROOT/artifacts"
  rm -f "$TMP_ROOT/events.log"
  mkdir -p "$TMP_ROOT/artifacts"
}

set_manifest_target_loadable() {
  local path="$1"
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
manifest["scenarios"][0]["visible"] = {
    "target": "products",
    "object": "new-meta",
    "search": "loadable",
}
manifest["scenarios"][0]["digests"]["target"] = "sha256:c4e0333b19d6fef7e34f20221d21369e1c8eee2ea8e008c0caca416eb6dd6c1b"
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

set_manifest_target_digest() {
  local path="$1"
  local digest="$2"
  python3 - "$path" "$digest" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
manifest["scenarios"][0]["digests"]["target"] = sys.argv[2]
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

set_manifest_hidden_target_digest() {
  local path="$1"
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
manifest["scenarios"][0]["digests"]["target"] = (
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
)
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

add_manifest_non_string_projection_fields() {
  local path="$1"
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
fixture = manifest["live_http_fixture"]
for object_key in ["target_object", "old_target_object", "control_object"]:
    fixture[object_key]["body"].update({
        "rank": 7,
        "tags": ["repair", 2026, {"kind": "guide"}],
        "dimensions": {"width": 12, "units": "cm"},
    })
    # Known answers from the production highlighter: it renders array items as leaves
    # rather than descending, so the object item collapses to "{}".
    fixture[object_key]["expected_highlight"].update({
        "rank": "7",
        "tags[0]": "repair",
        "tags[1]": "2026",
        "tags[2]": "{}",
        "dimensions.width": "12",
        "dimensions.units": "cm",
    })
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

add_manifest_float_projection_field() {
  local path="$1"
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
fixture = manifest["live_http_fixture"]
for object_key in ["target_object", "old_target_object", "control_object"]:
    fixture[object_key]["body"].update({
        "rating": 7.0,
        "negative_zero_rating": -0.0,
        "tiny_rating": 1e-7,
    })
    # Known answers from Rust's f64 Display, which drops an integral float's fraction,
    # keeps negative zero signed, and never falls back to exponent notation. A Python
    # mirror renders the last two as "0" and "1e-07".
    fixture[object_key]["expected_highlight"].update({
        "rating": "7",
        "negative_zero_rating": "-0",
        "tiny_rating": "0.0000001",
    })
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

add_manifest_discarded_projection_fields() {
  local path="$1"
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
fixture = manifest["live_http_fixture"]
for object_key in ["target_object", "old_target_object", "control_object"]:
    body = fixture[object_key]["body"]
    body["enabled"] = True
    body["optional_note"] = None
    body["empty_tags"] = []
    body["empty_metadata"] = {}
    body["recursively_empty"] = {
        "flag": False,
        "nested": {"values": [None, True, [], {}]},
    }
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

set_manifest_quarantine_retains_empty_staging() {
  local path="$1"
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
scenario = manifest["scenarios"][0]
scenario["disposition"] = "quarantine"
scenario["cli"] = {"status": "quarantined", "action": "quarantine", "exit_code": 2}
scenario["residue"] = {
    "staging": "present",
    "backup": "absent",
    "journal": "absent",
    "quarantine": "present",
}
scenario["digests"]["new"] = (
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
)
scenario["digests"]["staging"] = "absent"
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

set_manifest_clean_disposition_to_quarantine_oracle() {
  local path="$1"
  python3 - "$path" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
manifest = json.loads(path.read_text(encoding="utf-8"))
scenario = manifest["scenarios"][0]
scenario["disposition"] = "quarantine"
scenario["cli"] = {"status": "quarantined", "action": "quarantine", "exit_code": 2}
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

write_destructive_ambiguity_manifest() {
  local path="$1"
  python3 - "$REPO_DIR/engine/tests/publication_repair_cli_scenarios.json" "$path" <<'PY'
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
selected_ids = {"base_020_replacement", "mutation_ambiguous_target_and_staging"}
scenarios = [scenario for scenario in source["scenarios"] if scenario["id"] in selected_ids]
if {scenario["id"] for scenario in scenarios} != selected_ids:
    raise SystemExit("canonical destructive-ambiguity scenarios are missing")
mutation = next(
    scenario for scenario in scenarios
    if scenario["id"] == "mutation_ambiguous_target_and_staging"
)
mutation["residue"]["quarantine"] = "absent"
filtered = dict(source)
filtered["layout_count"] = len(scenarios)
filtered["scenarios"] = scenarios
pathlib.Path(sys.argv[2]).write_text(json.dumps(filtered, indent=2), encoding="utf-8")
PY
}

write_manifest() {
  local path="$1"
  local layout_count="${2:-1}"
  local disposition="${3:-absent-create}"
  python3 - "$path" "$layout_count" "$disposition" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
layout_count = int(sys.argv[2])
disposition = sys.argv[3]
if disposition == "quarantine":
    cli = {"status": "quarantined", "action": "quarantine", "exit_code": 2}
    residue = {
        "staging": "absent",
        "backup": "absent",
        "journal": "absent",
        "quarantine": "present",
    }
else:
    cli = {"status": "clean", "action": "none", "exit_code": 0}
    residue = {
        "staging": "absent",
        "backup": "absent",
        "journal": "absent",
        "quarantine": "absent",
    }
manifest = {
    "schema_version": 1,
    "layout_count": layout_count,
    "live_http_fixture": {
        "target_index": "products",
        "control_index": "control_products",
        "target_object": {
            "object_id": "new-widget",
            "body": {
                "objectID": "new-widget",
                "title": "modern waffle iron",
                "body": "new repair guide",
                "generation": "new",
            },
            "expected_highlight": {
                "title": "modern waffle iron",
                "body": "new repair guide",
                "generation": "new",
            },
        },
        "old_target_object": {
            "object_id": "old-widget",
            "body": {
                "objectID": "old-widget",
                "title": "legacy waffle iron",
                "body": "old repair guide",
                "generation": "old",
            },
            "expected_highlight": {
                "title": "legacy waffle iron",
                "body": "old repair guide",
                "generation": "old",
            },
        },
        "control_object": {
            "object_id": "control-widget",
            "body": {
                "objectID": "control-widget",
                "title": "control waffle iron",
                "body": "unchanged control guide",
                "generation": "control",
            },
            "expected_highlight": {
                "title": "control waffle iron",
                "body": "unchanged control guide",
                "generation": "control",
            },
        },
        "target_query": {"text": "modern", "ordered_hit_ids": ["new-widget"]},
        "old_target_query": {"text": "legacy", "ordered_hit_ids": ["old-widget"]},
        "control_query": {"text": "control", "ordered_hit_ids": ["control-widget"]},
        "surface_statuses": {
            "index_absent": {
                "status": 404,
                "body": {"status": 404, "message": "Index 'products' does not exist"},
            },
            "object_absent": {"status": 404, "body": {"status": 404, "message": "Object not found"}},
            "object_unavailable": {"status": 503, "body": {"status": 503, "message": "Index unavailable"}},
            "search_unavailable": {"status": 503, "body": {"status": 503, "message": "Index unavailable"}},
        },
        "target_projections": {
            "new-meta": {"object": "target_object", "query": "target_query"},
            "old-meta": {"object": "old_target_object", "query": "old_target_query"},
        },
        "expectations": {
            "target_present": {"target": "present", "object": "present", "search": "present"},
            "target_absent": {"target": "absent", "object": "absent", "search": "absent"},
            "target_unavailable": {"target": "absent", "object": "unavailable", "search": "unavailable"},
            "control_present": {"target": "present", "object": "present", "search": "present"},
        },
    },
    "scenarios": [{
        "id": "case_clean",
        "kind": "base",
        "activation": "create",
        "tenant": "products",
        "transaction": "txn_001",
        "journal_phase": "absent",
        "boundary": {"identity": "create|sync_dir:.publication/products/txn_001/staging|1"},
        "policy_keys": [],
        "digests": {
            "old": "absent",
            "new": "absent",
            "target": "absent",
            "staging": "absent",
            "backup": "absent",
        },
        "sidecars": {
            "query_suggestions": {
                "old": "absent",
                "new": "absent",
                "target": "absent",
                "staging": "absent",
                "backup": "absent",
            },
            "analytics": {
                "old": "absent",
                "new": "absent",
                "target": "absent",
                "staging": "absent",
                "backup": "absent",
            },
        },
        "disposition": disposition,
        "cli": cli,
        "visible": {"target": "absent", "object": "absent", "search": "unavailable"},
        "residue": residue,
    }],
}
path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
PY
}

write_fake_binary() {
  local path="$1"
  local revision="$2"
  local mode="${3:-ok}"
  local default_server_mode="${4:-${FAKE_SERVER_MODE:-healthy}}"
  cat >"$path" <<SH
#!/usr/bin/env bash
set -euo pipefail

revision="\${FAKE_BUILD_REVISION:-$revision}"
mode="\${FAKE_BINARY_MODE:-$mode}"
server_mode="\${FAKE_SERVER_MODE:-$default_server_mode}"
event_log="\${FAKE_EVENT_LOG:-$TMP_ROOT/events.log}"
SH
  cat >>"$path" <<'SH'

log_event() {
  printf '%s\n' "$*" >>"$event_log"
}

if [ "$#" -eq 2 ] && [ "$1" = "build-info" ] && [ "$2" = "--json" ]; then
  python3 - "$revision" <<'PY'
import json
import sys
print(json.dumps({
    "schemaVersion": 1,
    "version": "test",
    "revision": sys.argv[1],
    "revisionKnown": True,
    "dirty": None,
    "dirtyKnown": False,
    "workspaceDigest": "test-workspace-digest",
    "profile": "test-profile",
    "target": "test-target",
    "features": [],
    "capabilities": {"vectorSearch": False, "vectorSearchLocal": False},
}, separators=(",", ":")))
PY
  exit 0
fi

if [ "$mode" = "malformed" ]; then
  printf '{not-json\n'
  exit 0
fi

if [ "$#" -eq 4 ] && [ "$1" = "--data-dir" ] && [ "$3" = "--auto-port" ] && [ "$4" = "--no-auth" ]; then
  effective_server_mode="$server_mode"
  if [ "$server_mode" = "second-stable-index-primary-drift" ] &&
    [ -f "$event_log" ] && [ "$(grep -F -c 'server_start|' "$event_log" || true)" -ge 1 ]; then
    effective_server_mode="stable-index-primary-drift"
  fi
  log_event "server_start|pid=$$|data_dir=$2|argv=$*|mode=$effective_server_mode"
  if [ "$effective_server_mode" = "exit-before-banner" ]; then
    printf 'fake server exiting before banner\n' >&2
    log_event "server_exit_before_banner|pid=$$"
    exit 17
  fi
  python3 - "$effective_server_mode" "$event_log" "$revision" "$2" <<'PY'
import http.server
import json
import os
import pathlib
import signal
import socketserver
import sys
import time
import urllib.parse

mode, event_log, revision, data_dir = sys.argv[1], sys.argv[2], sys.argv[3], pathlib.Path(sys.argv[4])
recorded_pid = os.getpgid(0)
manifest = json.loads((__import__("pathlib").Path(event_log).parent / "manifest.json").read_text(encoding="utf-8"))
scenario = next(
    (candidate for candidate in manifest["scenarios"] if candidate["id"] == data_dir.name),
    manifest["scenarios"][0],
)
fixture = manifest["live_http_fixture"]

def log(message):
    with open(event_log, "a", encoding="utf-8") as handle:
        handle.write(message + "\n")

def response_body_for(path, method):
    parsed = urllib.parse.urlparse(path)
    route = parsed.path
    if method == "GET" and route == "/health":
        build = {
            "schemaVersion": 1,
            "version": "test",
            "revision": revision,
            "revisionKnown": True,
            "dirty": None,
            "dirtyKnown": False,
            "workspaceDigest": "test-workspace-digest",
            "profile": "test-profile",
            "target": "test-target",
            "features": [],
            "capabilities": {"vectorSearch": False, "vectorSearchLocal": False},
        }
        if mode == "wrong-health-build":
            build = dict(build)
            build["version"] = "wrong"
        return 200, {
            "status": "ok",
            "version": build["version"],
            "build": build,
            "uptime_secs": 1,
            "capabilities": build["capabilities"],
            "active_writers": 0,
            "max_concurrent_writers": 1,
            "facet_cache_entries": 0,
            "facet_cache_cap": 0,
            "heap_allocated_mb": 1,
            "system_limit_mb": 1024,
            "pressure_level": "normal",
            "allocator": "test",
            "tenants_loaded": 1,
        }
    if method == "GET" and route == "/health/ready":
        return 200, {"ready": True}
    if method == "GET" and route == "/1/indexes":
        names = [fixture["control_index"]]
        if scenario["visible"]["target"] != "absent":
            names.append(fixture["target_index"])
        if mode == "extra-publication-index":
            names.append(".publication/products")
        items = [{
            "name": name,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "entries": 1,
            "dataSize": 1,
            "fileSize": 1,
            "lastBuildTimeS": 0,
            "numberOfPendingTasks": 0,
            "pendingTask": False,
        } for name in sorted(names)]
        if mode == "stable-index-primary-drift":
            for item in items:
                item["primary"] = "drifted-primary"
        return 200, {"items": items, "nbPages": 1}
    parts = route.split("/")
    if len(parts) >= 4 and parts[:3] == ["", "1", "indexes"]:
        index = urllib.parse.unquote(parts[3])
        if len(parts) == 5 and parts[4] == "query" and method == "GET":
            return query_response(index)
        if len(parts) == 5 and method == "GET":
            object_id = urllib.parse.unquote(parts[4])
            return object_response(index, object_id)
    return 404, {"status": 404, "message": "Not found"}

def object_response(index, object_id):
    if index == fixture["control_index"] and object_id == fixture["control_object"]["object_id"]:
        manifest_body = fixture["control_object"]["body"]
        body = object_response_body(manifest_body)
        if mode == "wrong-control-object":
            body["title"] = "wrong"
        if mode == "leak-object-token":
            body["debug"] = ".publication/products/txn_001/staging"
        return 200, body
    visible = scenario["visible"]
    if index == fixture["target_index"] and visible["search"] == "loadable":
        projection = fixture["target_projections"][visible["object"]]
        manifest_body = fixture[projection["object"]]["body"]
        body = object_response_body(manifest_body)
        if mode == "wrong-target-object":
            body["title"] = "wrong"
        return 200, body
    if visible["target"] == "absent":
        status = fixture["surface_statuses"]["index_absent"]
    else:
        status = fixture["surface_statuses"]["object_absent" if visible["object"] == "absent" else "object_unavailable"]
    if mode == "wrong-target-status":
        return 200, {"unexpected": True}
    return status["status"], status["body"]

DISCARDED_FIELD_VALUE = object()

def normalize_document_field_value(value):
    if value is None or isinstance(value, bool):
        return DISCARDED_FIELD_VALUE
    if isinstance(value, (str, int, float)):
        return value
    if isinstance(value, list):
        normalized = [
            child
            for item in value
            if (child := normalize_document_field_value(item)) is not DISCARDED_FIELD_VALUE
        ]
        return normalized if normalized else DISCARDED_FIELD_VALUE
    if isinstance(value, dict):
        normalized = {
            key: child
            for key, item in value.items()
            if (child := normalize_document_field_value(item)) is not DISCARDED_FIELD_VALUE
        }
        return normalized if normalized else DISCARDED_FIELD_VALUE
    return DISCARDED_FIELD_VALUE

def production_document_body(body):
    normalized = {"objectID": body["objectID"]}
    for key, value in body.items():
        if key in {"_id", "objectID"}:
            continue
        normalized_value = normalize_document_field_value(value)
        if normalized_value is not DISCARDED_FIELD_VALUE:
            normalized[key] = normalized_value
    return normalized

def object_response_body(manifest_body):
    if mode in {"unnormalized-object", "unnormalized-object-and-highlight"}:
        return dict(manifest_body)
    return production_document_body(manifest_body)

def highlight_leaf(source, expected_value, query_text):
    """Render one leaf the way the production highlighter does.

    `expected_value` is the manifest's production rendering of the leaf, which this
    fake must not re-derive: Python cannot reproduce Rust's `f64` Display. Only text
    leaves are match-highlighted; every other leaf is emitted as a no-match.
    """
    entry = {"value": expected_value, "matchLevel": "none", "matchedWords": []}
    if not isinstance(source, str):
        return entry
    query = query_text.lower()
    if query and query in expected_value.lower():
        match_level = "full" if query == expected_value.lower() else "partial"
        entry["value"] = expected_value.replace(query_text, f"<em>{query_text}</em>")
        entry["matchLevel"] = match_level
        entry["matchedWords"] = [query_text]
        entry["fullyHighlighted"] = match_level == "full"
    return entry

PYTHON_NATIVE_FLOAT_MODES = {
    "python-native-negative-zero-highlight": "negative_zero_rating",
    "python-native-tiny-float-highlight": "tiny_rating",
}

def python_native_highlight(value):
    """The float rendering a naive Python mirror of the Rust highlighter would produce.

    Wrong by construction — negative zero loses its sign and small magnitudes gain
    exponent notation — so the live helper must reject it in favour of the oracle.
    """
    text = str(int(value)) if float(value).is_integer() else str(value)
    return {"value": text, "matchLevel": "none", "matchedWords": []}

def highlight_value(source, expected_highlight, path, query_text):
    # Mirrors the production highlighter's recursion: objects descend, arrays render
    # each item as a leaf, and anything else is a leaf itself.
    if isinstance(source, list):
        return [
            highlight_leaf(item, expected_highlight[f"{path}[{position}]"], query_text)
            for position, item in enumerate(source)
        ]
    if isinstance(source, dict):
        return {
            key: highlight_value(child, expected_highlight, f"{path}.{key}", query_text)
            for key, child in source.items()
        }
    return highlight_leaf(source, expected_highlight[path], query_text)

def production_hit(object_fixture, query_text):
    normalized_body = production_document_body(object_fixture["body"])
    expected_highlight = object_fixture["expected_highlight"]
    hit = dict(normalized_body)
    hit["_highlightResult"] = {
        key: highlight_value(value, expected_highlight, key, query_text)
        for key, value in normalized_body.items()
        if key != "objectID"
    }
    return hit

def hit_fixture(object_fixture, **extra_fields):
    """`object_fixture` plus extra string fields, used by drift selectors.

    Each extra field is a string, whose production highlight rendering is the string
    itself, so the oracle stays complete without re-deriving any value.
    """
    expected_highlight = dict(object_fixture["expected_highlight"])
    for key, value in extra_fields.items():
        if key != "objectID":
            expected_highlight[key] = value
    return {
        "body": dict(object_fixture["body"], **extra_fields),
        "expected_highlight": expected_highlight,
    }

def query_response(index):
    if index == fixture["control_index"]:
        query = fixture["control_query"]
        target_object = fixture["control_object"]
    elif index == fixture["target_index"] and scenario["visible"]["search"] == "loadable":
        projection = fixture["target_projections"][scenario["visible"]["object"]]
        query = fixture[projection["query"]]
        target_object = fixture[projection["object"]]
    else:
        if scenario["visible"]["target"] == "absent":
            status = fixture["surface_statuses"]["index_absent"]
        else:
            status = fixture["surface_statuses"]["search_unavailable"]
        if mode == "wrong-target-status":
            return 200, {"hits": []}
        return status["status"], status["body"]
    if mode == "reduced-search-hit":
        hits = [dict(target_object["body"])]
    elif mode == "missing-highlight-field":
        hit = production_hit(target_object, query["text"])
        hit["_highlightResult"].pop("body", None)
        hits = [hit]
    elif mode == "missing-non-string-highlight-field":
        hit = production_hit(target_object, query["text"])
        hit["_highlightResult"].pop("rank", None)
        hits = [hit]
    elif mode in PYTHON_NATIVE_FLOAT_MODES:
        field = PYTHON_NATIVE_FLOAT_MODES[mode]
        hit = production_hit(target_object, query["text"])
        hit["_highlightResult"][field] = python_native_highlight(target_object["body"][field])
        hits = [hit]
    elif mode == "unnormalized-object-and-highlight":
        hit = production_hit(target_object, query["text"])
        # Deliberately emit highlight entries for fields Document::from_json discards;
        # the helper must reject the extra keys, so the rendered value is immaterial.
        for key, value in target_object["body"].items():
            if key != "objectID" and key not in hit["_highlightResult"]:
                hit["_highlightResult"][key] = {
                    "value": str(value),
                    "matchLevel": "none",
                    "matchedWords": [],
                }
        hits = [hit]
    elif mode == "extra-hit-body-field":
        # An otherwise production-shaped hit carrying one field the manifest never
        # declared, to isolate the hit-body assertion from the highlight assertions.
        hit = production_hit(target_object, query["text"])
        hit["surplus"] = "unexpected"
        hits = [hit]
    else:
        hits = [production_hit(target_object, query["text"])]
    if mode == "reordered-extra-hits":
        hits = [
            production_hit(hit_fixture(target_object, objectID="extra-widget"), query["text"]),
            production_hit(target_object, query["text"]),
        ]
    if mode == "leak-hit-token":
        hits = [
            production_hit(
                hit_fixture(target_object, debug=".publication_quarantine/products"),
                query["text"],
            )
        ]
    timings = {
        "queue": 0,
        "search": 1,
        "highlight": 0,
        "total": 1,
    }
    payload = {
        "hits": hits,
        "nbHits": len(hits),
        "page": 0,
        "nbPages": 1 if hits else 0,
        "hitsPerPage": 20,
        "processingTimeMS": 1,
        "serverTimeMS": 1,
        "query": query["text"],
        "params": "",
        "exhaustive": {
            "nbHits": True,
            "typo": True,
            "facetValues": True,
            "rulesMatch": True,
        },
        "exhaustiveNbHits": True,
        "exhaustiveTypo": True,
        "index": index,
        "renderingContent": {},
        "serverUsed": "fake-publication-test",
        "_automaticInsights": False,
        "processingTimingsMS": timings,
    }
    if mode == "malformed-dynamic-field":
        payload["processingTimeMS"] = "fast"
    return 200, payload

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            log(f"health_request|pid={os.getpid()}|path={self.path}")
        if mode == "banner-then-endpoint-hang":
            time.sleep(300)
            return
        status, payload = response_body_for(self.path, "GET")
        body = json.dumps(payload, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        if self.path == "/health":
            log(f'health_response|body={body.decode()}')

    def log_message(self, fmt, *args):
        return

class Server(socketserver.TCPServer):
    allow_reuse_address = True

with Server(("127.0.0.1", 0), Handler) as server:
    port = server.server_address[1]
    def stop(signum, frame):
        if mode == "ignore-term":
            log(f"server_term_ignored|pid={recorded_pid}|signal={signum}")
            return
        log(f"server_term|pid={recorded_pid}|signal={signum}")
        raise SystemExit(0)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    log(f"server_bound|pid={recorded_pid}|port={port}")
    if mode == "malformed-banner":
        print("Local:      http://127.0.0.1:notaport", flush=True)
    elif mode == "no-banner":
        pass
    elif mode == "api-docs-decoy-no-local":
        print(f"  ->  API Docs:   http://127.0.0.1:{port}", flush=True)
        log(f"server_api_docs_decoy|pid={recorded_pid}|port={port}")
    else:
        if mode == "delayed-banner":
            time.sleep(0.5)
        print(f"  ->  Local:      http://127.0.0.1:{port}", flush=True)
        log(f"server_banner|pid={recorded_pid}|port={port}")
    try:
        server.serve_forever()
    finally:
        log(f"server_exit|pid={recorded_pid}")
PY
  exit $?
fi

if [ "$#" -eq 6 ] && [ "$1" = "--data-dir" ] && [ "$3" = "repair-publication" ] && [ "$4" = "--tenant" ] && [ "$6" = "--json" ]; then
  log_event "repair_start|data_dir=$2|tenant=$5|argv=$*"
  case_id="$(basename "$2")"
  repair_count="$(grep -F -c 'repair_start|' "$event_log" || true)"
  if [ "$case_id" = "base_020_replacement" ]; then
    python3 - "$2" "$5" "$repair_count" <<'PY'
import json
import pathlib
import shutil
import sys

case_root = pathlib.Path(sys.argv[1])
tenant = sys.argv[2]
repair_count = int(sys.argv[3])
transaction = "txn_001"
if repair_count == 1:
    namespace = case_root / ".publication" / tenant / transaction
    (namespace / "staging").replace(case_root / tenant)
    shutil.rmtree(namespace / "backup")
    shutil.rmtree(namespace / "sidecars")
    query_suggestions = case_root / ".query_suggestions"
    for suffix in [".json", ".log.jsonl", ".status.json"]:
        (query_suggestions / f"{tenant}{suffix}").unlink()
        (query_suggestions / f"publication_{transaction}{suffix}").replace(
            query_suggestions / f"{tenant}{suffix}"
        )
    analytics = case_root / "analytics"
    shutil.rmtree(analytics / tenant)
    (analytics / f"publication_{transaction}").replace(analytics / tenant)
    status, action, phase = "repaired", "complete", "prepared"
else:
    status, action, phase = "clean", "none", "committed"
print(json.dumps({
    "tenant": tenant,
    "status": status,
    "action": action,
    "transaction_id": transaction,
    "phase": phase,
    "evidence": f".publication/{tenant}/{transaction}",
}, separators=(",", ":")))
PY
    log_event "repair_exit|data_dir=$2|tenant=$5|status=0"
    exit 0
  fi
  if [ -f "$2/mutate_after_first" ] && [ "$repair_count" -ge 2 ]; then
    mutation_kind="$(cat "$2/mutate_after_first")"
    case "$mutation_kind" in
      target)
        mkdir -p "$2/$5"
        printf 'changed\n' >>"$2/$5/idempotence_mutation"
        ;;
      residue)
        mkdir -p "$2/.publication/$5/txn_001"
        printf 'changed\n' >>"$2/.publication/$5/txn_001/journal.json"
        ;;
      sidecar)
        mkdir -p "$2/.query_suggestions"
        printf 'changed\n' >>"$2/.query_suggestions/$5.json"
        ;;
      expected_absent)
        mkdir -p "$2/.publication/$5/txn_001/staging"
        printf 'changed\n' >"$2/.publication/$5/txn_001/staging/idempotence_mutation"
        ;;
      runtime)
        printf 'changed\n' >>"$2/runtime_server_noise"
        ;;
      yes)
        printf 'changed\n' >>"$2/idempotence_mutation"
        ;;
      *)
        printf 'unknown mutation kind: %s\n' "$mutation_kind" >&2
        exit 98
        ;;
    esac
  fi
  if [ "$mode" = "quarantine" ] || [ "$case_id" = "mutation_ambiguous_target_and_staging" ]; then
    mkdir -p "$2/.publication_quarantine/$5/txn_001"
    python3 - "$event_log" "$case_id" "$5" "$2/.publication_quarantine/$5/txn_001/journal.json" <<'PY'
import json
import pathlib
import sys

event_log, case_id, tenant, journal_path = sys.argv[1:]
manifest = json.loads((pathlib.Path(event_log).parent / "manifest.json").read_text(encoding="utf-8"))
scenario = next(
    (candidate for candidate in manifest["scenarios"] if candidate["id"] == case_id),
    manifest["scenarios"][0],
)
phase = scenario.get("journal_phase")
if phase == "absent":
    phase = None
pathlib.Path(journal_path).write_text(json.dumps({
    "target": tenant,
    "transaction_id": scenario.get("transaction", "txn_001"),
    "phase": "quarantined",
}, separators=(",", ":")), encoding="utf-8")
print(json.dumps({
    "tenant": tenant,
    "status": "quarantined",
    "action": "quarantine",
    "transaction_id": scenario.get("transaction", "txn_001"),
    "phase": phase,
    "evidence": f".publication/{tenant}/{scenario.get('transaction', 'txn_001')}",
}, separators=(",", ":")))
PY
    log_event "repair_exit|data_dir=$2|tenant=$5|status=2"
    exit 2
  fi
  python3 - "$5" <<'PY'
import json
import sys
print(json.dumps({
    "tenant": sys.argv[1],
    "status": "clean",
    "action": "none",
    "transaction_id": None,
    "phase": None,
    "evidence": None,
}, separators=(",", ":")))
PY
  log_event "repair_exit|data_dir=$2|tenant=$5|status=0"
  exit 0
fi

printf 'unexpected fake binary argv: %s\n' "$*" >&2
log_event "unexpected_argv|argv=$*"
exit 99
SH
  chmod +x "$path"
}

write_fake_cargo() {
  local path="$1"
  local generated_mode="${2:-${FAKE_GENERATED_MODE:-ok}}"
  local mutate_on_repair="${3:-no}"
  mutate_on_repair="${FAKE_GENERATED_MUTATE_ON_REPAIR:-$mutate_on_repair}"
  cat >"$path" <<SH
#!/usr/bin/env bash
set -euo pipefail
generated_mode="$generated_mode"
mutate_on_repair="$mutate_on_repair"
SH
  cat >>"$path" <<'SH'

[ "${1:-}" = "test" ] || { echo "unexpected cargo argv: $*" >&2; exit 99; }
root="${PUBLICATION_REPAIR_CLI_ARTIFACT_DIR:?}"
manifest="${PUBLICATION_REPAIR_CLI_MANIFEST:?}"
if [ -n "${FAKE_ARTIFACT_SENTINEL_NAME:-}" ]; then
  : >"$(dirname "$root")/$FAKE_ARTIFACT_SENTINEL_NAME"
fi
if [ "$generated_mode" = "setup-error" ]; then
  printf 'fake cargo setup error\n' >&2
  exit 91
fi
python3 - "$root" "$manifest" "$generated_mode" "$mutate_on_repair" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
manifest_path = pathlib.Path(sys.argv[2])
mode = sys.argv[3]
mutate = sys.argv[4]
manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
if mode == "bad-count":
    layouts = []
else:
    layouts = []
    for scenario in manifest["scenarios"]:
        layout = dict(scenario)
        layout["scenario_id"] = layout.pop("id")
        boundary = layout.pop("boundary", None)
        layout["boundaries"] = [boundary["identity"]] if boundary else []
        layouts.append(layout)
    if mode == "bad-digest":
        layouts[0]["digests"] = dict(layouts[0]["digests"])
        layouts[0]["digests"]["target"] = "sha256:" + ("0" * 64)
for scenario in manifest["scenarios"]:
    case = root / scenario["id"]
    case.mkdir(exist_ok=True)
    if scenario["visible"]["target"] != "absent" or scenario["digests"]["target"] != "absent":
        target = case / scenario.get("tenant", "products")
        target.mkdir(exist_ok=True)
        if scenario["visible"]["object"] != "absent":
            (target / "index_meta.json").write_text(scenario["visible"]["object"], encoding="utf-8")
    if mutate != "no":
        (case / "mutate_after_first").write_text(mutate, encoding="utf-8")
    if mode == "retained-staging":
        (case / ".publication" / scenario.get("tenant", "products") / scenario.get("transaction", "txn_001") / "staging").mkdir(parents=True, exist_ok=True)
    if mode == "retained-sidecar-staging":
        sidecar_staging = (
            case
            / ".publication"
            / scenario.get("tenant", "products")
            / scenario.get("transaction", "txn_001")
            / "staging"
            / ".query_suggestions"
        )
        sidecar_staging.mkdir(parents=True, exist_ok=True)
        (sidecar_staging / f"{scenario.get('tenant', 'products')}.json").write_text("changed", encoding="utf-8")
    if scenario["id"] == "mutation_ambiguous_target_and_staging":
        (case / "force_quarantine").write_text("1", encoding="utf-8")
(root / "generated_layouts.json").write_text(json.dumps(layouts), encoding="utf-8")
PY
SH
  chmod +x "$path"
}

run_runner() {
  local binary="$1"
  local manifest="$2"
  local artifact_dir="$3"
  shift 3
  PATH="$TMP_ROOT/bin:$PATH" \
    PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST=1 \
    PUBLICATION_REPAIR_CLI_TEST_HARNESS_PID="$$" \
    "$RUNNER" \
    --binary "$binary" \
    --manifest "$manifest" \
    --artifact-dir "$artifact_dir" "$@"
}

expect_failure() {
  local expected="$1"
  shift
  local log="$TMP_ROOT/failure.log"
  if "$@" >"$log" 2>&1; then
    cat "$log" >&2
    die "expected failure containing: $expected"
  fi
  grep -F "$expected" "$log" >/dev/null || {
    cat "$log" >&2
    die "failure did not contain: $expected"
  }
}

assert_log_contains() {
  local path="$1"
  local expected="$2"
  grep -F "$expected" "$path" >/dev/null || {
    [ ! -f "$path" ] || cat "$path" >&2
    die "expected log to contain: $expected"
  }
}

assert_event_contains() {
  local expected="$1"
  assert_log_contains "$TMP_ROOT/events.log" "$expected"
}

assert_event_not_contains() {
  local unexpected="$1"
  if [ -f "$TMP_ROOT/events.log" ] && grep -F "$unexpected" "$TMP_ROOT/events.log" >/dev/null; then
    cat "$TMP_ROOT/events.log" >&2
    die "unexpected event present: $unexpected"
  fi
}

assert_event_count() {
  local pattern="$1"
  local expected="$2"
  local actual="0"
  if [ -f "$TMP_ROOT/events.log" ]; then
    actual="$( (grep -F "$pattern" "$TMP_ROOT/events.log" || true) | wc -l | tr -d ' ')"
  fi
  [ "$actual" = "$expected" ] || {
    [ ! -f "$TMP_ROOT/events.log" ] || cat "$TMP_ROOT/events.log" >&2
    die "event count for '$pattern' was $actual, expected $expected"
  }
}

assert_identity_gate_not_reached() {
  [ ! -e "$TMP_ROOT/artifacts/.runner/build_info.json" ] ||
    die "test invoke mode validation reached identity_gate"
}

assert_failure_index() {
  local expected_original="$1"
  local expected_final="$2"
  local expected_phase="$3"
  local expected_generated="$4"
  local expected_first_repair="$5"
  local expected_disposition="$6"
  python3 - \
    "$TMP_ROOT/artifacts/.runner/failure_evidence/index.json" \
    "$expected_original" "$expected_final" "$expected_phase" \
    "$expected_generated" "$expected_disposition" \
    "$(git -C "$REPO_DIR" rev-parse HEAD)" \
    "$(if [ -n "$(git -C "$REPO_DIR" status --short)" ]; then printf dirty; else printf clean; fi)" <<'PY'
import json
import pathlib
import sys

(
    index_path, expected_original, expected_final, expected_phase,
    expected_generated, expected_disposition, expected_head, expected_porcelain,
) = sys.argv[1:]
index = json.loads(pathlib.Path(index_path).read_text(encoding="utf-8"))
expected_keys = {
    "schema_version", "status", "checkout", "build_info", "manifest_summary",
    "server", "artifacts", "cleanup",
}
if set(index) != expected_keys or index["schema_version"] != 1:
    raise SystemExit(f"failure index schema mismatch: {index!r}")
expected_status = {
    "original_exit_status": int(expected_original),
    "final_exit_status": int(expected_final),
    "failure_phase": expected_phase,
    "failure_reason": {"unavailable": "runner did not record a reason"},
}
if index["status"] != expected_status:
    raise SystemExit(f"failure status mismatch: {index['status']!r}")
if index["checkout"] != {"head": expected_head, "porcelain": expected_porcelain}:
    raise SystemExit(f"checkout summary mismatch: {index['checkout']!r}")
expected_build_info = {"availability": "available", "path": ".runner/build_info.json"}
if expected_phase == "identity_gate":
    expected_build_info = {
        "availability": "unavailable",
        "reason": "valid build info was not produced before failure",
    }
if index["build_info"] != expected_build_info:
    raise SystemExit(f"build-info summary mismatch: {index['build_info']!r}")
summary = index["manifest_summary"]
expected_dispositions = {expected_disposition: 1}
if summary["expected_count"] != 1 or summary["expected_dispositions"] != expected_dispositions:
    raise SystemExit(f"manifest expected summary mismatch: {summary!r}")
if expected_generated == "available":
    if summary["generated_observed_count"] != 1:
        raise SystemExit(f"generated observed count mismatch: {summary!r}")
    if summary["generated_dispositions"] != expected_dispositions:
        raise SystemExit(f"generated disposition summary mismatch: {summary!r}")
    if index["artifacts"]["generated_layouts"] != "generated/generated_layouts.json":
        raise SystemExit(f"generated path mismatch: {index['artifacts']!r}")
else:
    unavailable = {"unavailable": "generated layouts were not produced before failure"}
    if summary["generated_observed_count"] != unavailable:
        raise SystemExit(f"generated unavailable count mismatch: {summary!r}")
    if summary["generated_dispositions"] != unavailable:
        raise SystemExit(f"generated unavailable dispositions mismatch: {summary!r}")
if index["artifacts"]["cleanup_trace"] != ".runner/cleanup_trace.jsonl":
    raise SystemExit(f"cleanup trace reference mismatch: {index['artifacts']!r}")
PY
  assert_failure_index_retained_artifacts "$expected_first_repair"
}

assert_failure_index_retained_artifacts() {
  local expected_first_repair="$1"
  python3 - "$TMP_ROOT/artifacts/.runner/failure_evidence/index.json" \
    "$expected_first_repair" "$TMP_ROOT" "$REPO_DIR" <<'PY'
import json
import pathlib
import sys

index_path, expected_first_repair, tmp_root, repo_dir = sys.argv[1:]
index = json.loads(pathlib.Path(index_path).read_text(encoding="utf-8"))
first = index["artifacts"]["repair_invocations"]["first"]
second = index["artifacts"]["repair_invocations"]["second"]
if expected_first_repair == "available":
    expected_first = {
        "stdout": ".runner/case_clean.first.stdout.json",
        "stderr": ".runner/case_clean.first.stderr",
        "report": ".runner/case_clean.first.stdout.json",
    }
    if first != expected_first:
        raise SystemExit(f"first repair paths mismatch: {first!r}")
else:
    if set(value.get("unavailable") for value in first.values()) != {"not produced before failure"}:
        raise SystemExit(f"first repair unavailable reasons mismatch: {first!r}")
if set(value.get("unavailable") for value in second.values()) != {"not produced before failure"}:
    raise SystemExit(f"second repair unavailable reasons mismatch: {second!r}")
unavailable_server = {"unavailable": "server was not started before failure"}
if index["server"] != {
    "recorded_child_pid": unavailable_server,
    "bind_address": unavailable_server,
    "stdout": unavailable_server,
    "stderr": unavailable_server,
}:
    raise SystemExit(f"server unavailable summary mismatch: {index['server']!r}")
unavailable_artifact = {"unavailable": "not produced before failure"}
if index["artifacts"]["health_probe_stderr"] != unavailable_artifact:
    raise SystemExit(f"health probe unavailable reason mismatch: {index['artifacts']!r}")
if index["artifacts"]["http_projection_receipts"] != {
    "first": unavailable_artifact,
    "second": unavailable_artifact,
}:
    raise SystemExit(f"projection receipt unavailable reasons mismatch: {index['artifacts']!r}")
cleanup = index["cleanup"]
if cleanup["outcome"] != "succeeded":
    raise SystemExit(f"cleanup outcome mismatch: {cleanup!r}")
if cleanup["attempted_removals"] != [".runner/publication_repair_cli_live_helper.py"]:
    raise SystemExit(f"cleanup removal allowlist mismatch: {cleanup!r}")
serialized = json.dumps(index, sort_keys=True)
for forbidden in (tmp_root, repo_dir, "SECRET_SHOULD_NOT_APPEAR"):
    if forbidden in serialized:
        raise SystemExit(f"failure index leaked forbidden value: {forbidden}")
PY
}

wait_for_event() {
  local pattern="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))
  while :; do
    if [ -f "$TMP_ROOT/events.log" ] && grep -F "$pattern" "$TMP_ROOT/events.log" >/dev/null; then
      return 0
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      [ ! -f "$TMP_ROOT/events.log" ] || cat "$TMP_ROOT/events.log" >&2
      die "timed out waiting for event: $pattern"
    fi
    sleep 0.05
  done
}

latest_event_pid() {
  local pattern="$1"
  python3 - "$TMP_ROOT/events.log" "$pattern" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
pattern = sys.argv[2]
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
for line in reversed(events):
    if pattern not in line:
        continue
    match = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", line)
    if match:
        print(match.group(1))
        raise SystemExit(0)
raise SystemExit(f"missing pid for event containing {pattern!r}")
PY
}

assert_pid_stopped() {
  local pid="$1"
  local label="$2"
  local deadline=$((SECONDS + 3))
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      kill -9 "$pid" 2>/dev/null || true
      die "$label process $pid was still running"
    fi
    sleep 0.05
  done
}

assert_no_health_before_banner() {
  python3 - "$TMP_ROOT/events.log" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
banner = next((idx for idx, line in enumerate(events) if line.startswith("server_banner|")), None)
request = next((idx for idx, line in enumerate(events) if line.startswith("health_request|")), None)
if banner is None:
    raise SystemExit("missing canonical server banner event")
if request is None:
    raise SystemExit("missing /health request event after banner")
if request < banner:
    raise SystemExit("/health request occurred before canonical server banner")
PY
}

assert_converged_lifecycle_trace() {
  python3 - "$TMP_ROOT/events.log" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
kinds = [line.split("|", 1)[0] for line in events]
expected = [
    "repair_start",
    "repair_exit",
    "server_start",
    "server_bound",
    "server_banner",
    "health_request",
    "health_response",
    "health_request",
    "health_response",
    "server_term",
    "server_exit",
    "repair_start",
    "repair_exit",
    "server_start",
    "server_bound",
    "server_banner",
    "health_request",
    "health_response",
    "health_request",
    "health_response",
    "server_term",
    "server_exit",
]
if kinds != expected:
    raise SystemExit(
        "converged lifecycle event trace mismatch; expected first repair completion before server start/banner/health/stop and second repair completion before a distinct second server lifecycle, got "
        + repr(kinds)
    )
server_starts = [line for line in events if line.startswith("server_start|")]
if len(server_starts) != 2 or server_starts[0] == server_starts[1]:
    raise SystemExit("expected two distinct server invocations")
for line in server_starts:
    if "--data-dir " not in line or " --auto-port --no-auth" not in line:
        raise SystemExit("server invocation did not use exact --data-dir <case_root> --auto-port --no-auth argv: " + line)
PY
}

assert_term_ignored_kill_trace() {
  python3 - "$TMP_ROOT/events.log" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
kinds = [line.split("|", 1)[0] for line in events]
expected = [
    "repair_start",
    "repair_exit",
    "server_start",
    "server_bound",
    "server_banner",
    "health_request",
    "health_response",
    "health_request",
    "health_response",
    "server_term_ignored",
]
if kinds != expected:
    raise SystemExit("TERM-ignored lifecycle trace mismatch: " + repr(kinds))
bound = next(line for line in events if line.startswith("server_bound|"))
ignored = next(line for line in events if line.startswith("server_term_ignored|"))
bound_pid = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", bound).group(1)
ignored_pid = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", ignored).group(1)
if bound_pid != ignored_pid:
    raise SystemExit(f"TERM was not delivered to the bound server process: bound={bound_pid} ignored={ignored_pid}")
PY
}

assert_interrupted_cleanup_trace() {
  python3 - "$TMP_ROOT/events.log" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
events = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
kinds = [line.split("|", 1)[0] for line in events]
expected_prefix = [
    "repair_start",
    "repair_exit",
    "server_start",
    "server_bound",
    "server_banner",
    "health_request",
]
if kinds[:len(expected_prefix)] != expected_prefix:
    raise SystemExit("interrupted lifecycle prefix mismatch: " + repr(kinds))
terms = [line for line in events if line.startswith("server_term|")]
if len(terms) != 1:
    raise SystemExit("expected exactly one server_term during interrupted cleanup: " + repr(kinds))
bound = next(line for line in events if line.startswith("server_bound|"))
bound_pid = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", bound).group(1)
term_pid = re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", terms[0]).group(1)
if bound_pid != term_pid:
    raise SystemExit(f"interrupted cleanup targeted the wrong process: bound={bound_pid} term={term_pid}")
PY
}

assert_cleanup_trace_targets_child() {
  local require_kill="${1:-no}"
  local require_index="${2:-no}"
  python3 - "$TMP_ROOT/artifacts/.runner/cleanup_trace.jsonl" \
    "$TMP_ROOT/events.log" "$require_kill" "$require_index" <<'PY'
import json
import pathlib
import re
import sys

path, event_path, require_kill, require_index = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]), sys.argv[3], sys.argv[4]
events_log = event_path.read_text(encoding="utf-8").splitlines()
expected_pids = {
    int(re.search(r"(?:^|\|)pid=([0-9]+)(?:\||$)", line).group(1))
    for line in events_log if line.startswith("server_bound|")
}
if not expected_pids:
    raise SystemExit("event log recorded no server child PID")
records = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]
expected_keys = {
    "sequence", "event", "recorded_child_pid", "attempted_signal",
    "target_type", "target", "fallback_result", "result",
}
if not records or any(set(record) != expected_keys for record in records):
    raise SystemExit(f"cleanup trace schema mismatch: {records!r}")
if [record["sequence"] for record in records] != list(range(1, len(records) + 1)):
    raise SystemExit(f"cleanup trace sequence mismatch: {records!r}")
events = [record["event"] for record in records]
if "cleanup_started" not in events or events[-1] != "cleanup_finished":
    raise SystemExit(f"cleanup trace boundary mismatch: {events!r}")
if "state_cleared" not in events:
    raise SystemExit(f"cleanup trace omitted state clearing: {events!r}")
signal_records = [record for record in records if record["event"] == "signal_attempt"]
if not signal_records or signal_records[0]["attempted_signal"] != "TERM":
    raise SystemExit(f"cleanup trace omitted TERM attempt: {records!r}")
if require_kill == "yes" and not any(record["attempted_signal"] == "KILL" for record in signal_records):
    raise SystemExit(f"cleanup trace omitted KILL escalation: {records!r}")
for record in records:
    if record["recorded_child_pid"] is not None and record["recorded_child_pid"] not in expected_pids:
        raise SystemExit(f"cleanup trace recorded unrelated child: {record!r}")
    target = record["target"]
    allowed_targets = {str(pid) for pid in expected_pids} | {f"-{pid}" for pid in expected_pids}
    if target is not None and target not in allowed_targets:
        raise SystemExit(f"cleanup trace targeted unrelated process: {record!r}")
if require_index == "yes":
    index = json.loads(
        (path.parent / "failure_evidence" / "index.json").read_text(encoding="utf-8")
    )
    if index["artifacts"]["cleanup_trace"] != ".runner/cleanup_trace.jsonl":
        raise SystemExit("failure evidence does not reference the canonical cleanup trace")
PY
}

assert_signal_failure_index() {
  local expected_status="$1"
  local signal_name="$2"
  local expected_pid="$3"
  python3 - "$TMP_ROOT/artifacts/.runner/failure_evidence/index.json" \
    "$expected_status" "$signal_name" "$expected_pid" <<'PY'
import json
import pathlib
import sys

path, expected_status, signal_name, expected_pid = sys.argv[1:]
index = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
expected = {
    "original_exit_status": int(expected_status),
    "final_exit_status": int(expected_status),
    "failure_phase": "signal",
    "failure_reason": f"runner received {signal_name}",
}
if index["status"] != expected:
    raise SystemExit(f"signal failure status mismatch: {index['status']!r}")
if index["server"]["recorded_child_pid"] != int(expected_pid):
    raise SystemExit(f"signal evidence child mismatch: {index['server']!r}")
if not isinstance(index["server"]["bind_address"], str):
    raise SystemExit(f"signal evidence bind address unavailable: {index['server']!r}")
PY
}

assert_server_failure_evidence() {
  local expected_phase="$1"
  local expected_reason="$2"
  local bind_available="$3"
  local health_available="$4"
  python3 - "$TMP_ROOT/artifacts/.runner/failure_evidence/index.json" \
    "$expected_phase" "$expected_reason" "$bind_available" "$health_available" <<'PY'
import json
import pathlib
import sys

path, expected_phase, expected_reason, bind_available, health_available = sys.argv[1:]
index = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
status = index["status"]
expected_reason_value = (
    {"unavailable": "runner did not record a reason"}
    if expected_reason == "unavailable" else expected_reason
)
if status["failure_phase"] != expected_phase or status["failure_reason"] != expected_reason_value:
    raise SystemExit(f"server failure status mismatch: {status!r}")
server = index["server"]
if not isinstance(server["recorded_child_pid"], int):
    raise SystemExit(f"server child PID unavailable: {server!r}")
if bind_available == "yes":
    if not isinstance(server["bind_address"], str):
        raise SystemExit(f"server bind address unavailable: {server!r}")
else:
    expected = {"unavailable": "server bind address was not observed before failure"}
    if server["bind_address"] != expected:
        raise SystemExit(f"server bind unavailable reason mismatch: {server!r}")
expected_server_paths = {
    "stdout": ".runner/case_clean.first.server.stdout",
    "stderr": ".runner/case_clean.first.server.stderr",
}
for field, expected in expected_server_paths.items():
    if server[field] != expected:
        raise SystemExit(f"server {field} path mismatch: {server!r}")
health = index["artifacts"]["health_probe_stderr"]
if health_available == "yes":
    if health != ".runner/case_clean.health_probe.stderr":
        raise SystemExit(f"health stderr path mismatch: {health!r}")
elif health != {"unavailable": "not produced before failure"}:
    raise SystemExit(f"health stderr unavailable reason mismatch: {health!r}")
post_runner = index["cleanup"]["post_inventory"][".runner"]
for retained in expected_server_paths.values():
    if retained not in post_runner:
        raise SystemExit(f"post-cleanup inventory omitted retained server artifact: {retained}")
if ".runner/publication_repair_cli_live_helper.py" in post_runner:
    raise SystemExit("post-cleanup inventory retained transient helper")
if ".runner/publication_repair_cli_live_helper.py" not in index["cleanup"]["pre_inventory"][".runner"]:
    raise SystemExit("pre-cleanup inventory omitted transient helper")
PY
}

assert_filesystem_cleanup_index() {
  python3 - "$TMP_ROOT/artifacts/.runner/failure_evidence/index.json" <<'PY'
import json
import pathlib
import sys

index = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
cleanup = index["cleanup"]
if cleanup["attempted_removals"] != [".runner/publication_repair_cli_live_helper.py"]:
    raise SystemExit(f"cleanup attempted unexpected removal: {cleanup!r}")
pre_runner = cleanup["pre_inventory"][".runner"]
post_runner = cleanup["post_inventory"][".runner"]
if ".runner/publication_repair_cli_live_helper.py" not in pre_runner:
    raise SystemExit(f"pre-cleanup helper missing: {pre_runner!r}")
if ".runner/publication_repair_cli_live_helper.py" in post_runner:
    raise SystemExit(f"post-cleanup helper retained: {post_runner!r}")
required = {
    ".runner/build_info.json", ".runner/cleanup_trace.jsonl",
    ".runner/case_clean.first.stdout.json", ".runner/case_clean.first.stderr",
}
if not required.issubset(post_runner):
    raise SystemExit(f"post-cleanup runner inventory omitted retained evidence: {post_runner!r}")
if cleanup["pre_inventory"]["generated"] != cleanup["post_inventory"]["generated"]:
    raise SystemExit("cleanup changed generated artifacts")
if cleanup["pre_inventory"]["repair"] != cleanup["post_inventory"]["repair"]:
    raise SystemExit("cleanup changed repair artifacts")
if any(path.startswith(".runner/failure_evidence/") for path in pre_runner + post_runner):
    raise SystemExit("failure evidence recursively inventoried itself")
PY
}

test_argument_and_absolute_path_validation() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "binary must be an absolute path" \
    run_runner "relative/flapjack" "$manifest" "$TMP_ROOT/artifacts"
  touch "$TMP_ROOT/artifacts/leftover"
  expect_failure "artifact directory must be empty" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_invoke_mode_validation_precedes_identity_gate() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "unknown PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST: unknown" \
    env PATH="$TMP_ROOT/bin:$PATH" \
      PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST=1 \
      PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST=unknown \
      "$RUNNER" --binary "$binary" --manifest "$manifest" --artifact-dir "$TMP_ROOT/artifacts"
  assert_identity_gate_not_reached

  reset_artifacts
  expect_failure "PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST requires PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST=1" \
    env -u PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST \
      PATH="$TMP_ROOT/bin:$PATH" \
      PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST=skip_first_repair \
      "$RUNNER" --binary "$binary" --manifest "$manifest" --artifact-dir "$TMP_ROOT/artifacts"
  assert_identity_gate_not_reached

  reset_artifacts
  expect_failure "PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST is restricted to publication_repair_cli_live_test.sh" \
    env PATH="$TMP_ROOT/bin:$PATH" \
      PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST=1 \
      PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST=skip_first_repair \
      "$RUNNER" --binary "$binary" --manifest "$manifest" --artifact-dir "$TMP_ROOT/artifacts"
  assert_identity_gate_not_reached
}

test_manifest_generated_count_and_digest_mismatch_failures() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo" bad-count
  expect_failure "generated layout count 0 does not match manifest layout_count 1" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"

  reset_artifacts
  write_fake_cargo "$TMP_ROOT/bin/cargo" bad-digest
  expect_failure "layout case_clean digests mismatch" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_malformed_cli_json_and_idempotence_mutation_failures() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local selector=""
  local expected=""
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" malformed
  write_fake_cargo "$TMP_ROOT/bin/cargo"
  expect_failure "case_clean first CLI stdout is not valid JSON" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"

  for row in \
    "target|case_clean managed target changed after second repair" \
    "residue|case_clean managed residue journal changed after second repair" \
    "sidecar|case_clean managed sidecar query_suggestions.target changed after second repair" \
    "expected_absent|case_clean managed residue staging changed after second repair"
  do
    selector="${row%%|*}"
    expected="${row#*|}"
    reset_artifacts
    write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" ok
    write_fake_cargo "$TMP_ROOT/bin/cargo" ok "$selector"
    expect_failure "$expected" \
      run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  done

  reset_artifacts
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" ok
  write_fake_cargo "$TMP_ROOT/bin/cargo" ok runtime
  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/runtime_noise.log" 2>&1 || {
      cat "$TMP_ROOT/runtime_noise.log" >&2
      die "runtime-only noise should not affect managed publication idempotence"
    }
}

test_manifest_declared_nonzero_exit_does_not_abort_runner() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest" 1 quarantine
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/nonzero.log" 2>&1 || {
      cat "$TMP_ROOT/nonzero.log" >&2
      die "manifest-declared nonzero CLI exit aborted the runner"
    }
  grep -F "PASS: publication repair CLI live contract passed" "$TMP_ROOT/nonzero.log" >/dev/null || {
    cat "$TMP_ROOT/nonzero.log" >&2
    die "nonzero CLI scenario did not complete the live contract"
  }
}

test_converged_case_runs_server_after_each_repair() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest" 1 absent-create
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/converged.log" 2>&1 || {
      cat "$TMP_ROOT/converged.log" >&2
      die "converged lifecycle runner failed before server assertions"
    }
  assert_converged_lifecycle_trace
}

test_converged_case_rejects_second_stable_projection_drift() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest" 1 absent-create
  FAKE_SERVER_MODE=second-stable-index-primary-drift write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean stable HTTP projection changed after restart" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_event_count "server_start|" 2
  assert_event_count "server_term|" 2
  assert_cleanup_trace_targets_child no no
}

test_manifest_declared_nonzero_exit_runs_one_server_then_stops() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest" 1 quarantine
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/nonzero_server.log" 2>&1 || {
      cat "$TMP_ROOT/nonzero_server.log" >&2
      die "manifest-declared nonzero CLI exit aborted the runner before server assertions"
    }
  assert_event_count "repair_exit|" 1
  assert_event_count "server_start|" 1
  assert_event_not_contains "second repair"
}

test_banner_gates_health_probe() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=delayed-banner write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/banner_gating.log" 2>&1 || {
      cat "$TMP_ROOT/banner_gating.log" >&2
      die "banner-gating runner failed before ordering assertions"
    }
  assert_no_health_before_banner
  assert_event_contains 'health_response|body={"status":"ok","version":"test"'
}

test_http_projection_rejects_wrong_target_object() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  set_manifest_target_loadable "$manifest"
  FAKE_SERVER_MODE=wrong-target-object write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean products object body mismatch" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_state_assertion_rejects_manifest_target_digest_mismatch() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  set_manifest_target_loadable "$manifest"
  set_manifest_target_digest "$manifest" "sha256:0000000000000000000000000000000000000000000000000000000000000000"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean target digest" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_state_assertion_rejects_manifest_absent_target_digest_with_path() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  set_manifest_target_loadable "$manifest"
  set_manifest_target_digest "$manifest" "absent"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean target digest expected absent but path exists" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_state_assertion_allows_hidden_target_digest_with_gated_http_projection() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest" 1 quarantine
  set_manifest_hidden_target_digest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/hidden_target.log" 2>&1 || {
      cat "$TMP_ROOT/hidden_target.log" >&2
      die "hidden on-disk target should satisfy the gated HTTP projection contract"
    }
  grep -F "PASS: publication repair CLI live contract passed" "$TMP_ROOT/hidden_target.log" >/dev/null || {
    cat "$TMP_ROOT/hidden_target.log" >&2
    die "hidden target digest scenario did not complete the live contract"
  }
}

test_state_assertion_allows_retained_staging_digest_owned_by_new_digest() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  set_manifest_quarantine_retains_empty_staging "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo" retained-staging

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/retained_staging.log" 2>&1 || {
      cat "$TMP_ROOT/retained_staging.log" >&2
      die "retained staging residue should be validated against the manifest new digest"
    }
}

test_nonconverged_state_assertion_failure_fails_runner() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest" 1 quarantine
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo" retained-sidecar-staging

  expect_failure "case_clean staging digest expected absent but path exists" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_failure_evidence_lifecycle() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  set_manifest_target_loadable "$manifest"
  set_manifest_target_digest "$manifest" "sha256:0000000000000000000000000000000000000000000000000000000000000000"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"
  SECRET_FAILURE_EVIDENCE_PROBE=SECRET_SHOULD_NOT_APPEAR \
    expect_failure "case_clean target digest" run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_failure_index 1 1 state_assertion available available absent-create

  reset_artifacts
  write_manifest "$manifest" 1 quarantine
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo" retained-sidecar-staging
  expect_failure "case_clean staging digest expected absent but path exists" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_failure_index 1 1 state_assertion available available quarantine

  reset_artifacts
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo" setup-error
  expect_failure "fake cargo setup error" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_failure_index 91 91 generator unavailable unavailable absent-create

  reset_artifacts
  write_fake_cargo "$TMP_ROOT/bin/cargo"
cat >"$TMP_ROOT/bin/gtimeout" <<'SH'
#!/usr/bin/env bash
printf 'fake bounded timeout\n' >&2
exit 124
SH
  chmod +x "$TMP_ROOT/bin/gtimeout"
  expect_failure "fake bounded timeout" run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_failure_index 124 124 identity_gate unavailable unavailable absent-create

  reset_artifacts
  rm -f "$TMP_ROOT/bin/gtimeout"
  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" >"$TMP_ROOT/success.log" 2>&1 || {
    cat "$TMP_ROOT/success.log" >&2
    die "ordinary success failed during evidence lifecycle test"
  }
  [ ! -e "$TMP_ROOT/artifacts/.runner/failure_evidence/index.json" ] ||
    die "ordinary successful run published failure evidence"
}

test_http_projection_rejects_wrong_control_object() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=wrong-control-object write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean control_products object body mismatch" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_unavailable_status_mismatch() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=wrong-target-status write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "/1/indexes/products/new-widget status 200 does not match 404" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_reordered_extra_hits() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=reordered-extra-hits write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean control_products search hit count mismatch" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_accepts_production_hit_metadata() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=production-hit-metadata write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_accepts_non_string_highlight_metadata() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  add_manifest_non_string_projection_fields "$manifest"
  FAKE_SERVER_MODE=production-hit-metadata write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_accepts_rust_float_highlight_values() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  add_manifest_float_projection_field "$manifest"
  FAKE_SERVER_MODE=production-hit-metadata write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_python_native_negative_zero_highlight() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  add_manifest_float_projection_field "$manifest"
  FAKE_SERVER_MODE=python-native-negative-zero-highlight write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "_highlightResult.negative_zero_rating.value mismatch: '0' != '-0'" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_python_native_tiny_float_highlight() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  add_manifest_float_projection_field "$manifest"
  FAKE_SERVER_MODE=python-native-tiny-float-highlight write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "_highlightResult.tiny_rating.value mismatch: '1e-07' != '0.0000001'" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_discarded_object_fields() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  add_manifest_discarded_projection_fields "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean control_products object body mismatch" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_discarded_highlight_fields() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  add_manifest_discarded_projection_fields "$manifest"
  FAKE_SERVER_MODE=unnormalized-object write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean control_products hit 0 _highlightResult missing keys" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_discarded_hit_body_fields() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  add_manifest_discarded_projection_fields "$manifest"
  FAKE_SERVER_MODE=unnormalized-object-and-highlight write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  # A discarded field has no production highlight rendering, so once the fake serves the
  # unnormalized object and highlight, the leaf-value assertion is what rejects it.
  expect_failure "case_clean control_products hit 0._highlightResult.enabled has no manifest-owned expected highlight value" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_extra_hit_body_field() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=extra-hit-body-field write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean control_products hit 0 body mismatch" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_reduced_search_hit_metadata() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=reduced-search-hit write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean control_products hit 0 missing required hit metadata _highlightResult" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_missing_highlight_field() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=missing-highlight-field write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean control_products hit 0 _highlightResult missing keys" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_missing_non_string_highlight_field() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  add_manifest_non_string_projection_fields "$manifest"
  FAKE_SERVER_MODE=missing-non-string-highlight-field write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean control_products hit 0 _highlightResult missing keys" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_wrong_health_build() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=wrong-health-build write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "/health version mismatch" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_malformed_dynamic_field() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=malformed-dynamic-field write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean control_products processingTimeMS must be a non-negative integer" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_http_projection_rejects_publication_like_index() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=extra-publication-index write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "user projection leaked forbidden token .publication" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_server_failure_evidence http_projection unavailable yes yes
}

test_http_projection_rejects_object_and_hit_leaks() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=leak-object-token write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"
  expect_failure "user projection leaked forbidden token .publication" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"

  reset_artifacts
  FAKE_SERVER_MODE=leak-hit-token write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"
  expect_failure "user projection leaked forbidden token .publication" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
}

test_exact_child_cleanup_preserves_sentinel() {
  make_workspace
  local sentinel_pid=""
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  sleep 300 &
  sentinel_pid=$!
  trap 'kill "$sentinel_pid" 2>/dev/null || true; cleanup' EXIT
  write_manifest "$manifest"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/cleanup.log" 2>&1 || {
      cat "$TMP_ROOT/cleanup.log" >&2
      kill "$sentinel_pid" 2>/dev/null || true
      die "cleanup runner failed before exact-child assertions"
    }
  assert_event_count "server_term|" 2
  kill -0 "$sentinel_pid" 2>/dev/null || die "sentinel process was not preserved"
  kill "$sentinel_pid" 2>/dev/null || true
  wait "$sentinel_pid" 2>/dev/null || true
  trap cleanup EXIT
}

test_term_ignored_server_escalates_to_kill() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local server_pid=""
  local started_at="$SECONDS"
  write_manifest "$manifest" 1 quarantine
  FAKE_SERVER_MODE=ignore-term write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/term_ignored.log" 2>&1 || {
      cat "$TMP_ROOT/term_ignored.log" >&2
      die "TERM-ignored server runner failed before escalation assertions"
    }
  server_pid="$(latest_event_pid "server_bound|")"
  assert_term_ignored_kill_trace
  assert_cleanup_trace_targets_child yes no
  assert_pid_stopped "$server_pid" "TERM-ignored server"
  [ $((SECONDS - started_at)) -le 9 ] || die "TERM-to-KILL escalation exceeded bounded shutdown"
}

test_post_kill_group_timeout_fails_closed() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local bash_env="$TMP_ROOT/force_post_kill_group_alive.sh"
  local post_kill_marker="$TMP_ROOT/post_kill.signal"
  write_manifest "$manifest" 1 quarantine
  FAKE_SERVER_MODE=ignore-term write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"
  cat >"$bash_env" <<'SH'
kill() {
  if [ "${1:-}" = "-KILL" ] && [ "${2:-}" = "--" ]; then
    builtin kill "$@"
    : >"$FORCE_POST_KILL_GROUP_ALIVE_MARKER"
    return 0
  fi
  if [ "${1:-}" = "-0" ] && [ "${2:-}" = "--" ] &&
    [ -f "$FORCE_POST_KILL_GROUP_ALIVE_MARKER" ]; then
    return 0
  fi
  builtin kill "$@"
}
SH

  BASH_ENV="$bash_env" FORCE_POST_KILL_GROUP_ALIVE_MARKER="$post_kill_marker" \
    expect_failure "server process group remained alive after KILL" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  [ -f "$post_kill_marker" ] || die "post-KILL process-group timeout seam did not observe KILL"
  assert_event_contains "server_term_ignored|"
  assert_cleanup_trace_targets_child yes yes
}

test_post_kill_unreapable_child_fails_within_budget() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local bash_env="$TMP_ROOT/force_child_unreapable.sh"
  local kill_marker="$TMP_ROOT/unreapable_child.signal"
  local started_at="$SECONDS"
  write_manifest "$manifest" 1 quarantine
  FAKE_SERVER_MODE=ignore-term write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)" quarantine
  write_fake_cargo "$TMP_ROOT/bin/cargo"
  # After the real KILL is delivered, force the direct-child liveness probe
  # (`kill -0 <pid>`, no group `--`) to keep reporting the child alive so the
  # recorded child can never be reaped. A blocking `wait` would stall shutdown
  # forever; the bounded reap must instead give up and fail closed on budget.
  cat >"$bash_env" <<'SH'
kill() {
  if [ "${1:-}" = "-KILL" ]; then
    builtin kill "$@"
    : >"$FORCE_UNREAPABLE_CHILD_MARKER"
    return 0
  fi
  if [ "${1:-}" = "-0" ] && [ "$#" -eq 2 ] && [ "${2#-}" = "$2" ] &&
    [ -f "$FORCE_UNREAPABLE_CHILD_MARKER" ]; then
    return 0
  fi
  builtin kill "$@"
}
SH

  BASH_ENV="$bash_env" FORCE_UNREAPABLE_CHILD_MARKER="$kill_marker" \
    expect_failure "server child could not be reaped within the bounded shutdown budget after KILL" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  [ -f "$kill_marker" ] || die "post-KILL unreapable-child seam did not observe KILL"
  [ $((SECONDS - started_at)) -le 9 ] || die "post-KILL unreapable-child shutdown exceeded bounded budget"
  assert_event_contains "server_term_ignored|"
  assert_cleanup_trace_targets_child yes yes
}

test_interrupted_run_cleans_child_group_and_preserves_sentinel() {
  make_workspace
  local sentinel_pid=""
  local runner_pid=""
  local runner_status=0
  local watchdog_pid=""
  local timeout_marker="$TMP_ROOT/interrupted_runner_timeout"
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local server_pid=""
  sleep 300 &
  sentinel_pid=$!
  trap 'kill "$sentinel_pid" 2>/dev/null || true; cleanup' EXIT
  write_manifest "$manifest"
  FAKE_SERVER_MODE=banner-then-endpoint-hang write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  PATH="$TMP_ROOT/bin:$PATH" python3 - "$RUNNER" "$binary" "$manifest" "$TMP_ROOT/artifacts" \
    >"$TMP_ROOT/interrupted.log" 2>&1 <<'PY' &
import os
import signal
import sys

os.setsid()
signal.signal(signal.SIGINT, signal.SIG_DFL)
runner, binary, manifest, artifact_dir = sys.argv[1:]
os.environ["PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST"] = "1"
os.execv(
    runner,
    [
        runner,
        "--binary", binary,
        "--manifest", manifest,
        "--artifact-dir", artifact_dir,
    ],
)
PY
  runner_pid=$!
  wait_for_event "health_request|" 5
  server_pid="$(latest_event_pid "server_bound|")"
  kill -INT -- "-$runner_pid"
  python3 - "$runner_pid" "$timeout_marker" <<'PY' &
import os
import pathlib
import signal
import sys
import time

runner_pid = int(sys.argv[1])
timeout_marker = pathlib.Path(sys.argv[2])
time.sleep(2)
try:
    os.kill(runner_pid, 0)
except ProcessLookupError:
    raise SystemExit(0)
timeout_marker.touch()
os.kill(runner_pid, signal.SIGTERM)
PY
  watchdog_pid=$!
  set +e
  wait "$runner_pid"
  runner_status=$?
  kill "$watchdog_pid" 2>/dev/null
  wait "$watchdog_pid" 2>/dev/null
  set -e
  [ ! -f "$timeout_marker" ] || die "interrupted runner did not exit promptly after SIGINT"
  [ "$runner_status" -eq 130 ] || die "interrupted runner exit status was $runner_status, expected SIGINT status 130"
  assert_interrupted_cleanup_trace
  assert_cleanup_trace_targets_child no yes
  assert_signal_failure_index 130 INT "$server_pid"
  assert_pid_stopped "$server_pid" "interrupted server"
  kill -0 "$sentinel_pid" 2>/dev/null || die "sentinel process was not preserved"
  kill "$sentinel_pid" 2>/dev/null || true
  wait "$sentinel_pid" 2>/dev/null || true
  trap cleanup EXIT
}

test_signal_exit_statuses_preserve_evidence() {
  local signal_name=""
  local expected_status=""
  for signal_name in INT TERM HUP; do
    case "$signal_name" in
      INT) expected_status=130 ;;
      TERM) expected_status=143 ;;
      HUP) expected_status=129 ;;
    esac
    make_workspace
    local sentinel_pid=""
    local runner_pid=""
    local runner_status=0
    local watchdog_pid=""
    local manifest="$TMP_ROOT/manifest.json"
    local binary="$TMP_ROOT/fake-flapjack"
    local server_pid=""
    local timeout_marker="$TMP_ROOT/${signal_name}.timeout"
    sleep 300 &
    sentinel_pid=$!
    trap 'kill "$sentinel_pid" 2>/dev/null || true; cleanup' EXIT
    write_manifest "$manifest"
    FAKE_SERVER_MODE=banner-then-endpoint-hang \
      write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
    write_fake_cargo "$TMP_ROOT/bin/cargo"
    PATH="$TMP_ROOT/bin:$PATH" python3 - "$RUNNER" "$binary" "$manifest" "$TMP_ROOT/artifacts" \
      >"$TMP_ROOT/${signal_name}.log" 2>&1 <<'PY' &
import os
import signal
import sys

os.setsid()
signal.signal(signal.SIGINT, signal.SIG_DFL)
runner, binary, manifest, artifact_dir = sys.argv[1:]
os.environ["PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST"] = "1"
os.execv(runner, [
    runner, "--binary", binary, "--manifest", manifest,
    "--artifact-dir", artifact_dir,
])
PY
    runner_pid=$!
    wait_for_event "health_request|" 5
    server_pid="$(latest_event_pid "server_bound|")"
    kill "-$signal_name" -- "-$runner_pid"
    python3 - "$runner_pid" "$timeout_marker" <<'PY' &
import os
import pathlib
import signal
import sys
import time

time.sleep(5)
try:
    os.kill(int(sys.argv[1]), 0)
except ProcessLookupError:
    raise SystemExit(0)
pathlib.Path(sys.argv[2]).touch()
os.kill(int(sys.argv[1]), signal.SIGKILL)
PY
    watchdog_pid=$!
    set +e
    wait "$runner_pid"
    runner_status=$?
    kill "$watchdog_pid" 2>/dev/null
    wait "$watchdog_pid" 2>/dev/null
    set -e
    [ ! -f "$timeout_marker" ] || die "$signal_name runner exceeded bounded completion"
    [ "$runner_status" -eq "$expected_status" ] ||
      die "$signal_name runner exit status was $runner_status, expected $expected_status"
    assert_pid_stopped "$server_pid" "$signal_name server"
    kill -0 "$sentinel_pid" 2>/dev/null || die "$signal_name cleanup killed unrelated sentinel"
    assert_cleanup_trace_targets_child no yes
    assert_signal_failure_index "$expected_status" "$signal_name" "$server_pid"
    kill "$sentinel_pid" 2>/dev/null || true
    wait "$sentinel_pid" 2>/dev/null || true
    trap cleanup EXIT
    cleanup
    TMP_ROOT=""
  done
}

test_server_early_exit() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=exit-before-banner write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean server exited before startup banner (status 17)" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_log_contains "$TMP_ROOT/failure.log" \
    "stdout: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stdout stderr: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stderr"
  assert_event_contains "server_exit_before_banner|"
  assert_server_failure_evidence server_startup \
    "case_clean server exited before startup banner (status 17); stdout: <artifact>/.runner/case_clean.first.server.stdout stderr: <artifact>/.runner/case_clean.first.server.stderr" \
    no no
}

test_malformed_server_banner() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=malformed-banner write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean timed out waiting for startup banner" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_event_count "health_request|" 0
  assert_event_contains "server_term|"
  assert_server_failure_evidence server_startup \
    "case_clean timed out waiting for startup banner; stdout: <artifact>/.runner/case_clean.first.server.stdout stderr: <artifact>/.runner/case_clean.first.server.stderr" \
    no no
}

test_decoy_api_docs_url_requires_local_banner() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=api-docs-decoy-no-local write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean timed out waiting for startup banner" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_event_contains "server_api_docs_decoy|"
  assert_event_count "health_request|" 0
  assert_event_contains "server_term|"
  assert_server_failure_evidence server_startup \
    "case_clean timed out waiting for startup banner; stdout: <artifact>/.runner/case_clean.first.server.stdout stderr: <artifact>/.runner/case_clean.first.server.stderr" \
    no no
}

test_server_startup_timeout() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=no-banner write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean timed out waiting for startup banner" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_log_contains "$TMP_ROOT/failure.log" \
    "stdout: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stdout stderr: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stderr"
  assert_event_count "health_request|" 0
  assert_event_contains "server_term|"
  assert_server_failure_evidence server_startup \
    "case_clean timed out waiting for startup banner; stdout: <artifact>/.runner/case_clean.first.server.stdout stderr: <artifact>/.runner/case_clean.first.server.stderr" \
    no no
}

test_server_endpoint_timeout() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  write_manifest "$manifest"
  FAKE_SERVER_MODE=banner-then-endpoint-hang write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  expect_failure "case_clean /health probe timed out" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  assert_log_contains "$TMP_ROOT/failure.log" \
    "stdout: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stdout stderr: $TMP_ROOT/artifacts/.runner/case_clean.first.server.stderr"
  assert_no_health_before_banner
  assert_event_contains "server_term|"
  assert_server_failure_evidence server_health \
    "case_clean /health probe timed out; stdout: <artifact>/.runner/case_clean.first.server.stdout stderr: <artifact>/.runner/case_clean.first.server.stderr" \
    yes yes
}

test_filesystem_cleanup_scope() {
  make_workspace
  local manifest="$TMP_ROOT/manifest.json"
  local binary="$TMP_ROOT/fake-flapjack"
  local outside_sentinel="$TMP_ROOT/outside_sentinel"
  local inside_sentinel="$TMP_ROOT/artifacts/operator_sentinel"
  : >"$outside_sentinel"
  write_manifest "$manifest"
  set_manifest_target_loadable "$manifest"
  set_manifest_target_digest "$manifest" "sha256:0000000000000000000000000000000000000000000000000000000000000000"
  write_fake_binary "$binary" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$TMP_ROOT/bin/cargo"

  FAKE_ARTIFACT_SENTINEL_NAME=operator_sentinel expect_failure "case_clean target digest" \
    run_runner "$binary" "$manifest" "$TMP_ROOT/artifacts"
  [ -f "$outside_sentinel" ] || die "cleanup removed sentinel outside artifact directory"
  [ -f "$inside_sentinel" ] || die "cleanup removed non-runner-owned artifact sentinel"
  [ ! -e "$TMP_ROOT/artifacts/.runner/publication_repair_cli_live_helper.py" ] ||
    die "cleanup retained transient embedded helper"
  assert_filesystem_cleanup_index
}

run_closed_mutation_row() {
  local name="$1"
  local expected="$2"
  local setup_func="$3"
  local planned_ref="$4"
  local executed_ref="$5"
  local setup_failures_ref="$6"
  local row_log="$TMP_ROOT/${name}.setup.log"
  local invoke_mode="${ROW_INVOKE_MODE:-}"

  printf -v "$planned_ref" '%s' "$((${!planned_ref} + 1))"
  reset_artifacts
  ROW_BINARY="$TMP_ROOT/fake-flapjack"
  ROW_MANIFEST="$TMP_ROOT/manifest.json"
  ROW_CARGO="$TMP_ROOT/bin/cargo"
  ROW_INVOKE_MODE=""
  if ! "$setup_func" >"$row_log" 2>&1; then
    cat "$row_log" >&2
    printf -v "$setup_failures_ref" '%s' "$((${!setup_failures_ref} + 1))"
    return
  fi
  printf -v "$executed_ref" '%s' "$((${!executed_ref} + 1))"
  invoke_mode="${ROW_INVOKE_MODE:-}"
  if [ -n "$invoke_mode" ]; then
    PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST="$invoke_mode" \
      expect_failure "$expected" run_runner "$ROW_BINARY" "$ROW_MANIFEST" "$TMP_ROOT/artifacts"
  else
    expect_failure "$expected" run_runner "$ROW_BINARY" "$ROW_MANIFEST" "$TMP_ROOT/artifacts"
  fi
}

setup_closed_row_stale_build_identity() {
  write_manifest "$ROW_MANIFEST"
  write_fake_binary "$ROW_BINARY" "1111111111111111111111111111111111111111"
  write_fake_cargo "$ROW_CARGO"
}

setup_closed_row_cli_bypass() {
  write_manifest "$ROW_MANIFEST"
  write_fake_binary "$ROW_BINARY" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$ROW_CARGO"
  ROW_INVOKE_MODE="skip_first_repair"
}

setup_closed_row_altered_expected_digest() {
  write_manifest "$ROW_MANIFEST"
  set_manifest_target_loadable "$ROW_MANIFEST"
  set_manifest_target_digest "$ROW_MANIFEST" "sha256:0000000000000000000000000000000000000000000000000000000000000000"
  write_fake_binary "$ROW_BINARY" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$ROW_CARGO"
}

setup_closed_row_altered_disposition() {
  write_manifest "$ROW_MANIFEST"
  set_manifest_clean_disposition_to_quarantine_oracle "$ROW_MANIFEST"
  write_fake_binary "$ROW_BINARY" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$ROW_CARGO"
}

setup_closed_row_omitted_generated_layout() {
  write_manifest "$ROW_MANIFEST"
  write_fake_binary "$ROW_BINARY" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$ROW_CARGO" bad-count
}

setup_closed_row_destructive_ambiguity() {
  write_destructive_ambiguity_manifest "$ROW_MANIFEST"
  write_fake_binary "$ROW_BINARY" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  rm -f "$ROW_CARGO"
}

setup_closed_row_hidden_work_http_leakage() {
  write_manifest "$ROW_MANIFEST"
  FAKE_SERVER_MODE=extra-publication-index write_fake_binary "$ROW_BINARY" "$(git -C "$REPO_DIR" rev-parse HEAD)"
  write_fake_cargo "$ROW_CARGO"
}

test_closed_mutation_matrix_fails_closed() {
  make_workspace
  local planned=0
  local executed=0
  local setup_failures=0

  run_closed_mutation_row stale_build_identity \
    "revision must match reviewed HEAD" \
    setup_closed_row_stale_build_identity planned executed setup_failures
  run_closed_mutation_row cli_bypass \
    "repair-publication subprocess invocation missing" \
    setup_closed_row_cli_bypass planned executed setup_failures
  run_closed_mutation_row altered_expected_digest \
    "case_clean target digest" \
    setup_closed_row_altered_expected_digest planned executed setup_failures
  run_closed_mutation_row altered_disposition \
    "case_clean CLI exit code 0 does not match manifest 2" \
    setup_closed_row_altered_disposition planned executed setup_failures
  run_closed_mutation_row omitted_generated_layout \
    "generated layout count 0 does not match manifest layout_count 1" \
    setup_closed_row_omitted_generated_layout planned executed setup_failures
  run_closed_mutation_row destructive_ambiguity \
    "mutation_ambiguous_target_and_staging residue quarantine should be absent" \
    setup_closed_row_destructive_ambiguity planned executed setup_failures
  run_closed_mutation_row hidden_work_http_leakage \
    "user projection leaked forbidden token .publication" \
    setup_closed_row_hidden_work_http_leakage planned executed setup_failures

  [ "$planned" -ne 0 ] || die "closed mutation matrix planned zero rows"
  [ "$setup_failures" -eq 0 ] || die "closed mutation matrix had $setup_failures setup failures"
  [ "$executed" -eq "$planned" ] || die "closed mutation matrix executed $executed of $planned planned rows"
}

run_test_function() {
  local name="$1"
  case "$name" in
    closed_mutation_matrix_fails_closed) test_closed_mutation_matrix_fails_closed ;;
    argument_and_absolute_path_validation) test_argument_and_absolute_path_validation ;;
    invoke_mode_validation_precedes_identity_gate) test_invoke_mode_validation_precedes_identity_gate ;;
    manifest_generated_count_and_digest_mismatch_failures) test_manifest_generated_count_and_digest_mismatch_failures ;;
    malformed_cli_json_and_idempotence_mutation_failures) test_malformed_cli_json_and_idempotence_mutation_failures ;;
    manifest_declared_nonzero_exit_does_not_abort_runner) test_manifest_declared_nonzero_exit_does_not_abort_runner ;;
    converged_case_runs_server_after_each_repair) test_converged_case_runs_server_after_each_repair ;;
    converged_case_rejects_second_stable_projection_drift) test_converged_case_rejects_second_stable_projection_drift ;;
    manifest_declared_nonzero_exit_runs_one_server_then_stops) test_manifest_declared_nonzero_exit_runs_one_server_then_stops ;;
    banner_gates_health_probe) test_banner_gates_health_probe ;;
    http_projection_rejects_wrong_target_object) test_http_projection_rejects_wrong_target_object ;;
    state_assertion_rejects_manifest_target_digest_mismatch) test_state_assertion_rejects_manifest_target_digest_mismatch ;;
    state_assertion_rejects_manifest_absent_target_digest_with_path) test_state_assertion_rejects_manifest_absent_target_digest_with_path ;;
    state_assertion_allows_hidden_target_digest_with_gated_http_projection) test_state_assertion_allows_hidden_target_digest_with_gated_http_projection ;;
    state_assertion_allows_retained_staging_digest_owned_by_new_digest) test_state_assertion_allows_retained_staging_digest_owned_by_new_digest ;;
    nonconverged_state_assertion_failure_fails_runner) test_nonconverged_state_assertion_failure_fails_runner ;;
    failure_evidence_lifecycle) test_failure_evidence_lifecycle ;;
    http_projection_rejects_wrong_control_object) test_http_projection_rejects_wrong_control_object ;;
    http_projection_rejects_unavailable_status_mismatch) test_http_projection_rejects_unavailable_status_mismatch ;;
    http_projection_rejects_reordered_extra_hits) test_http_projection_rejects_reordered_extra_hits ;;
    http_projection_accepts_production_hit_metadata) test_http_projection_accepts_production_hit_metadata ;;
    http_projection_accepts_non_string_highlight_metadata) test_http_projection_accepts_non_string_highlight_metadata ;;
    http_projection_accepts_rust_float_highlight_values) test_http_projection_accepts_rust_float_highlight_values ;;
    http_projection_rejects_python_native_negative_zero_highlight) test_http_projection_rejects_python_native_negative_zero_highlight ;;
    http_projection_rejects_python_native_tiny_float_highlight) test_http_projection_rejects_python_native_tiny_float_highlight ;;
    http_projection_rejects_discarded_object_fields) test_http_projection_rejects_discarded_object_fields ;;
    http_projection_rejects_discarded_highlight_fields) test_http_projection_rejects_discarded_highlight_fields ;;
    http_projection_rejects_discarded_hit_body_fields) test_http_projection_rejects_discarded_hit_body_fields ;;
    http_projection_rejects_extra_hit_body_field) test_http_projection_rejects_extra_hit_body_field ;;
    http_projection_rejects_reduced_search_hit_metadata) test_http_projection_rejects_reduced_search_hit_metadata ;;
    http_projection_rejects_missing_highlight_field) test_http_projection_rejects_missing_highlight_field ;;
    http_projection_rejects_missing_non_string_highlight_field) test_http_projection_rejects_missing_non_string_highlight_field ;;
    http_projection_rejects_wrong_health_build) test_http_projection_rejects_wrong_health_build ;;
    http_projection_rejects_malformed_dynamic_field) test_http_projection_rejects_malformed_dynamic_field ;;
    http_projection_rejects_publication_like_index) test_http_projection_rejects_publication_like_index ;;
    http_projection_rejects_object_and_hit_leaks) test_http_projection_rejects_object_and_hit_leaks ;;
    exact_child_cleanup_preserves_sentinel) test_exact_child_cleanup_preserves_sentinel ;;
    term_ignored_server_escalates_to_kill) test_term_ignored_server_escalates_to_kill ;;
    post_kill_group_timeout_fails_closed) test_post_kill_group_timeout_fails_closed ;;
    post_kill_unreapable_child_fails_within_budget) test_post_kill_unreapable_child_fails_within_budget ;;
    interrupted_run_cleans_child_group_and_preserves_sentinel) test_interrupted_run_cleans_child_group_and_preserves_sentinel ;;
    signal_exit_statuses_preserve_evidence) test_signal_exit_statuses_preserve_evidence ;;
    server_early_exit) test_server_early_exit ;;
    malformed_server_banner) test_malformed_server_banner ;;
    decoy_api_docs_url_requires_local_banner) test_decoy_api_docs_url_requires_local_banner ;;
    server_startup_timeout) test_server_startup_timeout ;;
    server_endpoint_timeout) test_server_endpoint_timeout ;;
    filesystem_cleanup_scope) test_filesystem_cleanup_scope ;;
    *) die "unknown test selector: $name" ;;
  esac
}

main() {
  local selected=""
  local tests=(
    argument_and_absolute_path_validation
    invoke_mode_validation_precedes_identity_gate
    closed_mutation_matrix_fails_closed
    manifest_generated_count_and_digest_mismatch_failures
    malformed_cli_json_and_idempotence_mutation_failures
    manifest_declared_nonzero_exit_does_not_abort_runner
    converged_case_runs_server_after_each_repair
    converged_case_rejects_second_stable_projection_drift
    manifest_declared_nonzero_exit_runs_one_server_then_stops
    banner_gates_health_probe
    http_projection_rejects_wrong_target_object
    state_assertion_rejects_manifest_target_digest_mismatch
    state_assertion_rejects_manifest_absent_target_digest_with_path
    state_assertion_allows_hidden_target_digest_with_gated_http_projection
    state_assertion_allows_retained_staging_digest_owned_by_new_digest
    failure_evidence_lifecycle
    http_projection_rejects_wrong_control_object
    http_projection_rejects_unavailable_status_mismatch
    http_projection_rejects_reordered_extra_hits
    http_projection_accepts_production_hit_metadata
    http_projection_accepts_non_string_highlight_metadata
    http_projection_accepts_rust_float_highlight_values
    http_projection_rejects_python_native_negative_zero_highlight
    http_projection_rejects_python_native_tiny_float_highlight
    http_projection_rejects_discarded_object_fields
    http_projection_rejects_discarded_highlight_fields
    http_projection_rejects_discarded_hit_body_fields
    http_projection_rejects_extra_hit_body_field
    http_projection_rejects_reduced_search_hit_metadata
    http_projection_rejects_missing_highlight_field
    http_projection_rejects_missing_non_string_highlight_field
    http_projection_rejects_wrong_health_build
    http_projection_rejects_malformed_dynamic_field
    http_projection_rejects_publication_like_index
    http_projection_rejects_object_and_hit_leaks
    exact_child_cleanup_preserves_sentinel
    term_ignored_server_escalates_to_kill
    post_kill_group_timeout_fails_closed
    post_kill_unreapable_child_fails_within_budget
    interrupted_run_cleans_child_group_and_preserves_sentinel
    signal_exit_statuses_preserve_evidence
    server_early_exit
    malformed_server_banner
    decoy_api_docs_url_requires_local_banner
    server_startup_timeout
    server_endpoint_timeout
    filesystem_cleanup_scope
  )
  [ "$#" -eq 0 ] || tests=("$@")
  for selected in "${tests[@]}"; do
    run_test_function "$selected"
    cleanup
    TMP_ROOT=""
  done
  if [ "$#" -eq 0 ]; then
    printf 'PASS: publication repair CLI live runner focused tests passed\n'
  else
    printf 'PASS: selected publication repair CLI live runner focused tests passed\n'
  fi
}

main "$@"
