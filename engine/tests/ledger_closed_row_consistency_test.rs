//! Fail the build when a ledger's open-work surfaces still present a row that
//! `ROADMAP.md` has already marked closed.
//!
//! WHY THIS EXISTS
//! ---------------
//! `ledger_correction_gate_test.rs` bounds how long a receipt's correction can go
//! unabsorbed. It deliberately does not check *content* — a citation proves
//! someone touched the ledger, not that every surface agrees afterwards. This
//! test covers the failure that gap allows, and it is not hypothetical: on
//! 2026-08-04 at `571458d37`, `ROADMAP.md` row `SEC-W2A` read
//! "**CLOSED 2026-08-04 at `fd656649e`**" and `engine/docs2/FEATURES.md` and
//! `docs/security/CONTROLS.md` both recorded ACME hot rotation as shipped, while
//! `PROJECT_OVERVIEW.md` still listed the ACME serving half as strategic priority
//! 2 and told operators that "automated ACME needs a reverse proxy or a restart."
//! `ROADMAP.md`'s own dispatch table still listed `SEC-W2A` as in flight. A
//! reader following the priority list would have planned work that had shipped
//! two days earlier, and every reading gate in the repository was green.
//!
//! THE RULE, STATED AS A CLOSED SET
//! --------------------------------
//! Single ownership decides this cleanly. `ROADMAP.md` owns row state, so a row
//! it marks closed is closed; `PROJECT_OVERVIEW.md` owns *priority order over
//! open work*, and `ROADMAP.md`'s `## Up Next` table owns *dispatch candidates*.
//! Neither surface has any reason to name a closed row — not as a priority, not
//! as a candidate, and not as a restated status, because restating row state is
//! the duplication that lets the two copies drift apart in the first place. So
//! the rule is: **no closed row ID appears in either open-work surface.**
//!
//! That is why the check is an ID membership test rather than a prose match. A
//! text match against narrative would rot on the first honest rewording; an ID
//! either is or is not listed among open work, and the fix is always the same
//! one edit.
//!
//! WHAT IT DELIBERATELY DOES NOT CHECK
//! -----------------------------------
//! It does not verify that a closed row's *replacement* text is right, and it
//! says nothing about surfaces that legitimately describe shipped state
//! (`engine/docs2/FEATURES.md`, `CHANGELOG.md`, `docs/security/CONTROLS.md`).
//! Those are shipped-status owners; naming a closed row there is correct.
//! Narrative outside the two open-work surfaces — including `## Up Next`'s note
//! recording which closed rows were removed from the table — is also untouched,
//! which is why the dispatch check reads table rows rather than the section body.
//!
//! NON-VACUITY
//! -----------
//! Named live specimen: at `571458d37` this test reports three findings —
//! `SEC-W2A` and `SEC-G3` in `PROJECT_OVERVIEW.md`'s `## Highest Priority`, and
//! `SEC-W2A` in `ROADMAP.md`'s `## Up Next` dispatch table — and fails. Marking
//! any currently-open row closed, or re-adding `SEC-W2A` to the priority list,
//! returns it to red today.

use std::fs;
use std::path::PathBuf;

/// The literal `ROADMAP.md` uses to declare a row closed. Bold is part of the
/// marker: rows discuss closure in prose ("Closed rows were removed…", "Closed or
/// moved to implemented…") far more often than they declare it, and the bold
/// status prefix is what a row uses to state its own verdict.
const CLOSED_MARKER: &str = "**CLOSED";

/// Sections that describe work still to be done. A closed row in either one is
/// the defect.
const OPEN_WORK_PRIORITY_SECTION: &str = "## Highest Priority";
const OPEN_WORK_DISPATCH_SECTION: &str = "## Up Next";

const LANDED_DATA_RECEIPT: &str =
    "engine/docs2/4_EVIDENCE/2026_08_03_aug03_11am_5_competitor_migration_lands_data_receipt.md";
const LANDED_DATA_MERGE: &str = "2c05776c7b9d8f60bae89c34ad819ece084fa2e4";

const NAMED_OPEN_ROWS: &[&str] = &["SEC-W4", "JOIN-1", "PL-10"];
const NAMED_CLOSED_ROWS: &[&str] = &[
    "BUILD-1",
    "DUR-1",
    "DUR-2",
    "SEC-G3",
    "SEC-W2A",
    "HA-MEMBERSHIP-UI",
    "MIG-17R",
];
const NAMED_SHIPPED_ROWS: &[&str] = &["INFRA-1"];
const NAMED_CROSS_OWNER_FACTS: &[&str] = &["SEC-G9", "PR-13"];
const NAMED_DENOMINATOR: usize = 13;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `engine/` for tests in this directory.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("engine/ always has a parent")
        .to_path_buf()
}

fn read_ledger(relative_path: &str) -> String {
    let path = repo_root().join(relative_path);
    // An unreadable ledger is a failure, not a skip: a gate that cannot find its
    // input must not report success.
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read ledger {}: {e}", path.display()))
}

/// The body of one `## `-level section, excluding its heading.
fn section_body<'a>(document: &'a str, heading: &str) -> &'a str {
    let after_heading = document
        .split_once(heading)
        .unwrap_or_else(|| panic!("no {heading:?} section — the ledger layout moved"))
        .1;
    match after_heading.find("\n## ") {
        Some(end) => &after_heading[..end],
        None => after_heading,
    }
}

/// The first cell of every markdown table row in `text`, minus header and
/// separator rows. Row IDs live there in both ledger tables.
fn table_row_ids(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(table_row_id)
        .map(str::to_string)
        .filter(|id| id != "ID" && !id.is_empty() && !id.starts_with('-'))
        .collect()
}

fn table_row_id(line: &str) -> Option<&str> {
    line.starts_with('|')
        .then(|| line.split('|').nth(1))
        .flatten()
        .map(|cell| cell.trim().trim_matches('`'))
}

fn table_row<'a>(document: &'a str, id: &str) -> Option<&'a str> {
    document
        .lines()
        .find(|line| table_row_id(line).is_some_and(|candidate| candidate == id))
}

fn table_row_state(row: &str) -> Option<&str> {
    row.split('|').nth(3).map(str::trim)
}

fn require_text(findings: &mut Vec<String>, owner: &str, text: &str, required: &str) {
    if !text.contains(required) {
        findings.push(format!(
            "  - {owner} is missing required claim {required:?}"
        ));
    }
}

/// Every `ROADMAP.md` row whose own status cell declares it closed.
fn closed_row_ids(roadmap: &str) -> Vec<String> {
    roadmap
        .lines()
        .filter(|line| line.starts_with('|'))
        .filter_map(|line| {
            let id = table_row_id(line)?;
            let state = table_row_state(line)?;
            state.contains(CLOSED_MARKER).then(|| id.to_string())
        })
        .filter(|id| !id.is_empty())
        .collect()
}

#[test]
fn closed_row_ids_reads_the_row_state_cell_only() {
    let roadmap = "\
| ID | Work Item | Current State | Evidence / Owner |
|----|-----------|---------------|------------------|
| DUR-1 | Durable admission under disk exhaustion | **OPEN — current ROADMAP.md row remains open.** | Evidence mentions the closed predecessor `SEC-W2A`: **CLOSED 2026-08-04 at `fd656649e`**. |
| SEC-W2A | Security wave 2 served TLS rotation | **CLOSED 2026-08-04 at `fd656649e`** | Served rotation receipt is present. |
";

    assert_eq!(closed_row_ids(roadmap), vec!["SEC-W2A".to_string()]);
}

/// Whether `text` names `id` as a standalone token.
///
/// Row IDs share prefixes — `SEC-W2` is a prefix of `SEC-W2A`, `MIG-17` of
/// `MIG-17R` — so a plain substring search would report a closed `SEC-W2` for
/// every mention of the open `SEC-W2A`. Both alphanumerics and `-` count as
/// interior characters, which is what makes those pairs distinguishable.
fn names_token(text: &str, id: &str) -> bool {
    let is_interior = |c: char| c.is_ascii_alphanumeric() || c == '-';
    text.match_indices(id).any(|(start, matched)| {
        let before_ok = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_interior(c));
        let after_ok = text[start + matched.len()..]
            .chars()
            .next()
            .is_none_or(|c| !is_interior(c));
        before_ok && after_ok
    })
}

#[test]
fn no_closed_roadmap_row_is_still_listed_as_open_work() {
    let roadmap = read_ledger("ROADMAP.md");
    let project_overview = read_ledger("PROJECT_OVERVIEW.md");

    let closed = closed_row_ids(&roadmap);
    // Guard the guard. If the status marker is reworded or the table layout
    // moves, an empty closed set would make this test pass by checking nothing —
    // the zero-denominator pass this repository's testing rules forbid. There is
    // no valid state of `ROADMAP.md` with no closed row in it.
    assert!(
        !closed.is_empty(),
        "no ROADMAP.md row declares {CLOSED_MARKER:?}. Either the closure marker was reworded \
         (update CLOSED_MARKER and the rows together) or the table layout moved. A \
         zero-denominator pass is not a pass."
    );

    let priority_section = section_body(&project_overview, OPEN_WORK_PRIORITY_SECTION);
    let dispatch_candidates = table_row_ids(section_body(&roadmap, OPEN_WORK_DISPATCH_SECTION));
    assert!(
        !dispatch_candidates.is_empty(),
        "ROADMAP.md {OPEN_WORK_DISPATCH_SECTION:?} has no candidate table rows. Either the \
         dispatch table moved or its layout changed; this check would otherwise pass by \
         reading an empty set."
    );

    let mut findings: Vec<String> = Vec::new();
    for id in &closed {
        if names_token(priority_section, id) {
            findings.push(format!(
                "  - `{id}` is closed in ROADMAP.md but still named in \
                 PROJECT_OVERVIEW.md {OPEN_WORK_PRIORITY_SECTION:?}"
            ));
        }
        if dispatch_candidates.iter().any(|candidate| candidate == id) {
            findings.push(format!(
                "  - `{id}` is closed in ROADMAP.md but still a dispatch candidate in its own \
                 {OPEN_WORK_DISPATCH_SECTION:?} table"
            ));
        }
    }

    assert!(
        findings.is_empty(),
        "{} closed ROADMAP.md row(s) are still presented as open work:\n{}\n\n\
         ROADMAP.md owns row state. PROJECT_OVERVIEW.md owns priority order over work that is \
         still open, and the {OPEN_WORK_DISPATCH_SECTION:?} table owns dispatch candidates — \
         neither should name a row that has closed. Remove the row from the open-work surface. \
         If the closed row's shipped behaviour still needs describing, that fact belongs to \
         engine/docs2/FEATURES.md, CHANGELOG.md, or docs/security/CONTROLS.md, not to a \
         priority list.\n\n\
         Do NOT satisfy this by softening the row's {CLOSED_MARKER:?} status in ROADMAP.md. \
         That reintroduces the contradiction this gate exists to catch, in the other direction.",
        findings.len(),
        findings.join("\n"),
    );
}

fn record_roadmap_state_conflicts(roadmap: &str, findings: &mut Vec<String>) {
    let roadmap_ids = table_row_ids(roadmap);
    let closed_ids = closed_row_ids(roadmap);

    for id in NAMED_OPEN_ROWS {
        if !roadmap_ids.iter().any(|candidate| candidate == id) {
            findings.push(format!("  - `{id}` is absent from ROADMAP.md"));
        } else if closed_ids.iter().any(|candidate| candidate == id) {
            findings.push(format!(
                "  - `{id}` must remain open but ROADMAP.md marks it closed"
            ));
        }
    }
    for id in NAMED_CLOSED_ROWS {
        if !closed_ids.iter().any(|candidate| candidate == id) {
            findings.push(format!("  - `{id}` must be closed in ROADMAP.md"));
        }
    }
    for id in NAMED_SHIPPED_ROWS {
        let state = table_row(roadmap, id).and_then(table_row_state);
        if !state.is_some_and(|value| value.starts_with("**SHIPPED")) {
            findings.push(format!("  - `{id}` must be shipped in ROADMAP.md"));
        }
    }
}

fn record_open_work_conflicts(
    priority_section: &str,
    dispatch_ids: &[String],
    findings: &mut Vec<String>,
) {
    for id in NAMED_CLOSED_ROWS
        .iter()
        .chain(NAMED_SHIPPED_ROWS)
        .chain(NAMED_CROSS_OWNER_FACTS)
    {
        if names_token(priority_section, id) {
            findings.push(format!(
                "  - `{id}` is closed/shipped but PROJECT_OVERVIEW.md still prioritizes it"
            ));
        }
        if dispatch_ids.iter().any(|candidate| candidate == id) {
            findings.push(format!(
                "  - `{id}` is closed/shipped but ROADMAP.md still dispatches it"
            ));
        }
    }
}

fn record_measured_owner_conflicts(roadmap: &str, features: &str, findings: &mut Vec<String>) {
    let join_row = table_row(roadmap, "JOIN-1").unwrap_or_default();
    require_text(findings, "ROADMAP.md row `JOIN-1`", join_row, "57 / 59");
    require_text(
        findings,
        "ROADMAP.md row `JOIN-1`",
        join_row,
        "2 capability-gated skips",
    );
    let sec_w4_row = table_row(roadmap, "SEC-W4").unwrap_or_default();
    require_text(
        findings,
        "ROADMAP.md row `SEC-W4`",
        sec_w4_row,
        "`SEC-G9` residuals closed",
    );
    let pr13_row = table_row(features, "PR-13").unwrap_or_default();
    require_text(findings, "FEATURES.md row `PR-13`", pr13_row, "✅ Done");
    require_text(
        findings,
        "FEATURES.md row `PR-13`",
        pr13_row,
        "tested 23 usable 23",
    );
}

fn record_migration_attribution_conflicts(
    roadmap: &str,
    features: &str,
    findings: &mut Vec<String>,
) {
    let migration_owners = [
        (
            "ROADMAP.md row `MIG-13`",
            table_row(roadmap, "MIG-13").unwrap_or_default(),
        ),
        (
            "ROADMAP.md row `MIG-14`",
            table_row(roadmap, "MIG-14").unwrap_or_default(),
        ),
        (
            "FEATURES.md source-migration owner",
            section_body(features, "## Source migration"),
        ),
    ];
    for (owner, text) in migration_owners {
        require_text(findings, owner, text, LANDED_DATA_RECEIPT);
        require_text(findings, owner, text, LANDED_DATA_MERGE);
        require_text(findings, owner, text, "local");
    }
    for id in ["MIG-13", "MIG-14"] {
        let row = table_row(roadmap, id).unwrap_or_default();
        for limitation in [
            "default-off",
            "settings",
            "synonyms",
            "rules",
            "permissions",
            "resume",
            "HA",
        ] {
            require_text(findings, &format!("ROADMAP.md row `{id}`"), row, limitation);
        }
    }
    let source_migration = migration_owners[2].1;
    require_text(
        findings,
        "FEATURES.md source-migration owner",
        source_migration,
        "Resume remains Algolia-only",
    );
    require_text(
        findings,
        "FEATURES.md source-migration owner",
        source_migration,
        "HA import is refused",
    );
}

#[test]
fn named_reconciliation_rejects_reopened_build_capacity_row() {
    let roadmap = read_ledger("ROADMAP.md");
    let build_row = table_row(&roadmap, "BUILD-1").expect("ROADMAP.md has a BUILD-1 row");
    let reopened_build_row = build_row.replacen(CLOSED_MARKER, "**OPEN", 1);
    let reopened_roadmap = roadmap.replacen(build_row, &reopened_build_row, 1);
    let mut findings = Vec::new();

    record_roadmap_state_conflicts(&reopened_roadmap, &mut findings);

    assert!(
        findings
            .iter()
            .any(|finding| finding == "  - `BUILD-1` must be closed in ROADMAP.md"),
        "reopening BUILD-1 must fail the named batch-row reconciliation"
    );
}

#[test]
fn named_batch_rows_are_reconciled_with_merged_evidence() {
    let roadmap = read_ledger("ROADMAP.md");
    let project_overview = read_ledger("PROJECT_OVERVIEW.md");
    let features = read_ledger("engine/docs2/FEATURES.md");
    let priority_section = section_body(&project_overview, OPEN_WORK_PRIORITY_SECTION);
    let dispatch_ids = table_row_ids(section_body(&roadmap, OPEN_WORK_DISPATCH_SECTION));
    let mut findings = Vec::new();

    let named_count = NAMED_OPEN_ROWS.len()
        + NAMED_CLOSED_ROWS.len()
        + NAMED_SHIPPED_ROWS.len()
        + NAMED_CROSS_OWNER_FACTS.len();
    assert_eq!(
        named_count, NAMED_DENOMINATOR,
        "the named reconciliation denominator changed; reconcile the complete closed set"
    );

    record_roadmap_state_conflicts(&roadmap, &mut findings);
    record_open_work_conflicts(priority_section, &dispatch_ids, &mut findings);
    record_measured_owner_conflicts(&roadmap, &features, &mut findings);
    record_migration_attribution_conflicts(&roadmap, &features, &mut findings);

    assert!(
        findings.is_empty(),
        "named batch-row reconciliation found {} conflict(s):\n{}",
        findings.len(),
        findings.join("\n")
    );
}
