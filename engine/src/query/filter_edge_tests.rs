use super::*;
use crate::error::FlapjackError;
use crate::index::schema::SchemaBuilder;
use crate::types::FieldValue;

fn make_compiler() -> FilterCompiler {
    let schema = SchemaBuilder::new().build();
    let tantivy = schema.to_tantivy();
    FilterCompiler::new(tantivy)
}

fn assert_invalid_query<T>(result: Result<T>) {
    match result {
        Err(FlapjackError::InvalidQuery(_)) => {}
        Err(other) => panic!("expected InvalidQuery, got {other:?}"),
        Ok(_) => panic!("expected InvalidQuery, got Ok result"),
    }
}

// ── format_value ────────────────────────────────────────────────────

#[test]
fn format_value_text_simple() {
    let c = make_compiler();
    assert_eq!(
        c.format_value(&FieldValue::Text("hello".into())).unwrap(),
        "hello"
    );
}

#[test]
fn format_value_text_with_spaces() {
    let c = make_compiler();
    assert_eq!(
        c.format_value(&FieldValue::Text("hello world".into()))
            .unwrap(),
        "\"hello world\""
    );
}

#[test]
fn format_value_integer() {
    let c = make_compiler();
    assert_eq!(c.format_value(&FieldValue::Integer(42)).unwrap(), "42");
}

#[test]
fn format_value_float() {
    let c = make_compiler();
    assert_eq!(c.format_value(&FieldValue::Float(2.5)).unwrap(), "2.5");
}

#[test]
fn format_value_facet() {
    let c = make_compiler();
    assert_eq!(
        c.format_value(&FieldValue::Facet("cat".into())).unwrap(),
        "\"cat\""
    );
}

#[test]
fn format_value_facet_with_inner_quote_escaped() {
    let c = make_compiler();
    assert_eq!(
        c.format_value(&FieldValue::Facet("elec\"tronics".into()))
            .unwrap(),
        "\"elec\\\"tronics\""
    );
}

#[test]
fn equals_query_string_for_value_integer_uses_range_syntax() {
    let c = make_compiler();
    assert_eq!(
        c.equals_query_string_for_value("price", &FieldValue::Integer(42))
            .unwrap(),
        "_json_filter.price:[42 TO 42]"
    );
}

#[test]
fn format_value_object_returns_error() {
    let c = make_compiler();
    let mut map = std::collections::HashMap::new();
    map.insert("key".to_string(), FieldValue::Text("val".into()));
    assert_invalid_query(c.format_value(&FieldValue::Object(map)));
}

#[test]
fn format_value_array_returns_error() {
    let c = make_compiler();
    let arr = vec![FieldValue::Integer(1), FieldValue::Integer(2)];
    assert_invalid_query(c.format_value(&FieldValue::Array(arr)));
}

#[test]
fn format_range_value_text_returns_error() {
    let c = make_compiler();
    assert_invalid_query(c.format_range_value(&FieldValue::Text("hello".into())));
}

#[test]
fn format_range_value_object_returns_error() {
    let c = make_compiler();
    let map = std::collections::HashMap::new();
    assert_invalid_query(c.format_range_value(&FieldValue::Object(map)));
}

// ── compile error paths ─────────────────────────────────────────────

#[test]
fn compile_equals_object_returns_error() {
    let c = make_compiler();
    let mut map = std::collections::HashMap::new();
    map.insert("key".to_string(), FieldValue::Text("val".into()));
    let f = Filter::Equals {
        field: "data".into(),
        value: FieldValue::Object(map),
    };
    assert_invalid_query(c.compile(&f, None));
}

#[test]
fn compile_not_equals_array_returns_error() {
    let c = make_compiler();
    let arr = vec![FieldValue::Integer(1)];
    let f = Filter::NotEquals {
        field: "tags".into(),
        value: FieldValue::Array(arr),
    };
    assert_invalid_query(c.compile(&f, None));
}

#[test]
fn compile_gte_text_returns_error() {
    let c = make_compiler();
    let f = Filter::GreaterThanOrEqual {
        field: "age".into(),
        value: FieldValue::Text("old".into()),
    };
    assert_invalid_query(c.compile(&f, None));
}

#[test]
fn compile_lte_text_returns_error() {
    let c = make_compiler();
    let f = Filter::LessThanOrEqual {
        field: "age".into(),
        value: FieldValue::Text("young".into()),
    };
    assert_invalid_query(c.compile(&f, None));
}

// ── equals / not-equals formatting parity ───────────────────────────

#[test]
fn equals_and_not_equals_integer_use_same_range_format() {
    let c = make_compiler();
    let eq_qs = c
        .to_query_string(&Filter::Equals {
            field: "price".into(),
            value: FieldValue::Integer(42),
        })
        .unwrap();
    let neq_inner = c
        .equals_query_string_for_value("price", &FieldValue::Integer(42))
        .unwrap();
    assert_eq!(eq_qs, neq_inner);
}

#[test]
#[allow(clippy::approx_constant)]
fn equals_and_not_equals_float_use_same_range_format() {
    let c = make_compiler();
    let eq_qs = c
        .to_query_string(&Filter::Equals {
            field: "score".into(),
            value: FieldValue::Float(3.14),
        })
        .unwrap();
    let neq_inner = c
        .equals_query_string_for_value("score", &FieldValue::Float(3.14))
        .unwrap();
    assert_eq!(eq_qs, neq_inner);
}

#[test]
fn equals_and_not_equals_date_use_same_range_format() {
    let c = make_compiler();
    let eq_qs = c
        .to_query_string(&Filter::Equals {
            field: "ts".into(),
            value: FieldValue::Date(1700000000),
        })
        .unwrap();
    let neq_inner = c
        .equals_query_string_for_value("ts", &FieldValue::Date(1700000000))
        .unwrap();
    assert_eq!(eq_qs, neq_inner);
}

// ── gt/lt edge cases ────────────────────────────────────────────────

#[test]
fn gt_float_uses_exclusive_bound() {
    let c = make_compiler();
    let f = Filter::GreaterThan {
        field: "price".into(),
        value: FieldValue::Float(10.5),
    };
    assert_eq!(
        c.to_query_string(&f).unwrap(),
        "_json_filter.price:{10.5 TO *]"
    );
}

#[test]
fn lt_float_uses_exclusive_bound() {
    let c = make_compiler();
    let f = Filter::LessThan {
        field: "price".into(),
        value: FieldValue::Float(10.5),
    };
    assert_eq!(
        c.to_query_string(&f).unwrap(),
        "_json_filter.price:[* TO 10.5}"
    );
}

#[test]
fn gt_integer_uses_exclusive_bound() {
    let c = make_compiler();
    let f = Filter::GreaterThan {
        field: "age".into(),
        value: FieldValue::Integer(18),
    };
    let qs = c.to_query_string(&f).unwrap();
    assert_eq!(qs, "_json_filter.age:{18 TO *]");
}

#[test]
fn lt_integer_uses_exclusive_bound() {
    let c = make_compiler();
    let f = Filter::LessThan {
        field: "age".into(),
        value: FieldValue::Integer(65),
    };
    let qs = c.to_query_string(&f).unwrap();
    assert_eq!(qs, "_json_filter.age:[* TO 65}");
}

// https://www.algolia.com/doc/api-reference/api-parameters/numericFilters
// Comparisons must apply to the value, regardless of the bound's JSON number type.
#[test]
fn numeric_inequalities_match_exact_mixed_integer_and_float_values() {
    use crate::index::Index;
    use crate::query::algolia_filters::parse_numeric_filter_string;
    use serde_json::json;
    use tantivy::collector::Count;

    let tmp = tempfile::TempDir::new().unwrap();
    let index = Index::create_in_dir(tmp.path()).unwrap();
    index
        .add_documents_simple(&[
            json!({"objectID":"negative", "rating":-4.5}),
            json!({"objectID":"four", "rating":4}),
            json!({"objectID":"below", "rating":4.499}),
            json!({"objectID":"half", "rating":4.5}),
            json!({"objectID":"above", "rating":4.501}),
            json!({"objectID":"five", "rating":5}),
        ])
        .unwrap();
    let compiler = FilterCompiler::new(index.schema().to_tantivy());
    let reader = index.reader();
    reader.reload().unwrap();
    let searcher = reader.searcher();
    for (expression, expected) in [
        ("rating > 4", 4),
        ("rating < 5", 5),
        ("rating >= 4", 5),
        ("rating <= 5", 6),
        ("rating > 4.4", 4),
        ("rating < 4.4", 2),
        ("rating > 4.5", 2),
        ("rating < 4.5", 3),
        ("rating >= 4.5", 3),
        ("rating <= 4.5", 4),
        ("rating > -4.5", 5),
        ("rating <= -4.5", 1),
        ("rating > 9223372036854775807", 0),
        ("rating < -9223372036854775808", 0),
    ] {
        let filter = parse_numeric_filter_string(expression).unwrap();
        let query = compiler.compile(&filter, None).unwrap();
        assert_eq!(
            searcher.search(&query, &Count).unwrap(),
            expected,
            "{expression}"
        );
        let negated = compiler
            .compile(&Filter::Not(Box::new(filter)), None)
            .unwrap();
        assert_eq!(
            searcher.search(&negated, &Count).unwrap(),
            6 - expected,
            "NOT {expression}"
        );
    }
}
