use crate::index::settings::IndexSettings;
use crate::query::parser::QueryParser;
use crate::types::{FieldValue, Filter, Query, ScoredDocument};
use serde_json::{json, Value};
use std::sync::Arc;
use tantivy::query::Query as TantivyQuery;
use tempfile::TempDir;

pub const EXPECTED_SEGMENT_COUNT: usize = 8;
/// Hand-counted documents across the eight parity batches:
/// 16 + 17 + 16 + 10 + 14 + 6 + 4 + 322.
pub const TOTAL_DOCS: usize = 405;
pub const SEARCH_LIMIT: usize = 20;
pub const PAGE_LIMIT: usize = 3;
pub const ALL_QUERY_FILTER_SCORE_BITS: u32 = 1_086_280_381;

pub struct ExecutorParityFixture {
    _temp_dir: TempDir,
    index: crate::Index,
    searcher: tantivy::Searcher,
    settings: Arc<IndexSettings>,
    searchable_paths: Vec<String>,
}

pub struct QuerySpec {
    pub name: &'static str,
    pub query: &'static str,
    pub query_type: &'static str,
    pub expected_ids: &'static [&'static str],
    pub expected_total: usize,
}

pub struct FacetExpectation {
    pub field: &'static str,
    pub values: &'static [(&'static str, u64)],
}

pub struct HighlightExpectation {
    pub document_id: &'static str,
    pub field: &'static str,
    pub value: &'static str,
}

type ProductSeed = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    i64,
    bool,
    i64,
    i64,
);

impl ExecutorParityFixture {
    pub fn segment_count(&self) -> usize {
        self.searcher.segment_readers().len()
    }

    pub fn searcher(&self) -> &tantivy::Searcher {
        &self.searcher
    }

    pub fn executor(&self, query_text: &str) -> crate::QueryExecutor {
        crate::QueryExecutor::new(self.index.converter(), self.index.inner().schema())
            .with_settings(Some(Arc::clone(&self.settings)))
            .with_query(query_text.to_string())
    }

    pub fn custom_ranking_executor(&self, query_text: &str) -> crate::QueryExecutor {
        let mut settings = (*self.settings).clone();
        settings.custom_ranking = Some(vec![
            "desc(popularity)".to_string(),
            "asc(price)".to_string(),
        ]);
        crate::QueryExecutor::new(self.index.converter(), self.index.inner().schema())
            .with_settings(Some(Arc::new(settings)))
            .with_query(query_text.to_string())
    }

    pub fn distinct_executor(
        &self,
        query_text: &str,
        distinct_attribute: &str,
    ) -> crate::QueryExecutor {
        let mut settings = (*self.settings).clone();
        settings.attribute_for_distinct = Some(distinct_attribute.to_string());
        crate::QueryExecutor::new(self.index.converter(), self.index.inner().schema())
            .with_settings(Some(Arc::new(settings)))
            .with_query(query_text.to_string())
    }

    pub fn text_query(&self, spec: &QuerySpec) -> Box<dyn TantivyQuery> {
        let schema = self.index.inner().schema();
        let json_search_field = schema.get_field("_json_search").unwrap();
        let json_exact_field = schema.get_field("_json_exact").unwrap();
        let parser = QueryParser::new_with_weights(
            &schema,
            vec![json_search_field],
            vec![1.0; self.searchable_paths.len()],
            self.searchable_paths.clone(),
        )
        .with_exact_field(json_exact_field)
        .with_query_type(spec.query_type)
        .with_typo_tolerance(true)
        .with_min_word_size_for_1_typo(self.settings.min_word_size_for_1_typo as usize)
        .with_min_word_size_for_2_typos(self.settings.min_word_size_for_2_typos as usize);
        let query = parser
            .parse(&Query {
                text: spec.query.to_string(),
            })
            .unwrap();
        self.executor(spec.query)
            .expand_short_query_with_searcher(query, &self.searcher)
            .unwrap()
    }
}

pub fn build_parity_fixture() -> ExecutorParityFixture {
    let temp_dir = TempDir::new().unwrap();
    let index = crate::Index::create_in_dir(temp_dir.path()).unwrap();
    let settings = Arc::new(parity_settings());
    let converter = index.converter();
    let batches = parity_batches();
    for batch in batches {
        let mut writer = index.writer().unwrap();
        for json_doc in batch {
            let document = crate::types::Document::from_json(&json_doc).unwrap();
            let tantivy_doc = converter
                .to_tantivy(&document, Some(settings.as_ref()))
                .unwrap();
            writer.add_document(tantivy_doc).unwrap();
        }
        writer.commit().unwrap();
    }

    let reader = index.reader();
    reader.reload().unwrap();
    let searcher = reader.searcher();
    let searchable_paths = settings.searchable_attributes.clone().unwrap();

    ExecutorParityFixture {
        _temp_dir: temp_dir,
        index,
        searcher,
        settings,
        searchable_paths,
    }
}

fn parity_settings() -> IndexSettings {
    IndexSettings {
        attributes_for_faceting: vec![
            "category".to_string(),
            "brand".to_string(),
            "tags".to_string(),
            "facetGroup".to_string(),
            "price".to_string(),
        ],
        searchable_attributes: Some(vec![
            "title".to_string(),
            "description".to_string(),
            "category".to_string(),
            "brand".to_string(),
        ]),
        ..IndexSettings::default()
    }
}

fn product_doc(
    id: &str,
    title: &str,
    category: &str,
    brand: &str,
    tags: &str,
    price: i64,
    in_stock: bool,
    release_year: i64,
    popularity: i64,
) -> Value {
    json!({
        "objectID": id,
        "title": title,
        "description": format!("{} parity specimen", title),
        "category": category,
        "brand": brand,
        "tags": tags,
        "facetGroup": "general",
        "price": price,
        "inStock": in_stock,
        "releaseYear": release_year,
        "popularity": popularity
    })
}

fn with_facet_group(mut doc: Value, group: &str) -> Value {
    doc.as_object_mut()
        .unwrap()
        .insert("facetGroup".to_string(), json!(group));
    doc
}

fn product_docs(seeds: &[ProductSeed]) -> Vec<Value> {
    seeds
        .iter()
        .map(
            |&(id, title, category, brand, tags, price, in_stock, release_year, popularity)| {
                product_doc(
                    id,
                    title,
                    category,
                    brand,
                    tags,
                    price,
                    in_stock,
                    release_year,
                    popularity,
                )
            },
        )
        .collect()
}

fn facet_group_docs(group: &str, seeds: &[ProductSeed]) -> Vec<Value> {
    product_docs(seeds)
        .into_iter()
        .map(|doc| with_facet_group(doc, group))
        .collect()
}

fn geo_doc(id: &str, lat: f64, lng: f64) -> Value {
    json!({
        "objectID": id,
        "title": "geoanchor depot",
        "description": "geoanchor parity specimen",
        "category": "Geo",
        "brand": "GeoBrand",
        "tags": "distance",
        "price": 1,
        "inStock": true,
        "releaseYear": 2024,
        "popularity": 1,
        "_geoloc": {"lat": lat, "lng": lng}
    })
}

fn filler_doc(batch: usize, idx: usize) -> Value {
    product_doc(
        &format!("zz_filler_{batch:02}_{idx:03}"),
        "catalog filler neutral WH1000 specimen",
        "Filler",
        "FillerBrand",
        "filler",
        10 + idx as i64,
        idx % 2 == 0,
        2020 + (idx % 4) as i64,
        idx as i64,
    )
}

fn wh_seed_doc(batch: usize) -> Value {
    product_doc(
        &format!("wh_seed_{batch:02}"),
        "WH1000 expansion seed",
        "Seed",
        "SeedBrand",
        "seed",
        1,
        true,
        2024,
        1,
    )
}

fn parity_batches() -> Vec<Vec<Value>> {
    vec![
        text_and_typo_docs(),
        multi_word_and_facet_docs(),
        filter_docs(),
        pagination_docs(),
        exact_hit_docs(),
        geo_and_highlight_docs(),
        custom_ranking_docs(),
        filler_docs(),
    ]
}

#[rustfmt::skip]
fn text_and_typo_docs() -> Vec<Value> {
    product_docs(&[
        ("t_dell_xps_01", "Dell XPS carbon laptop", "Laptop", "Dell", "business", 1400, false, 2023, 70),
        ("t_dell_xps_02", "Dell XPS studio laptop", "Laptop", "Dell", "creator", 1600, false, 2022, 68),
        ("t_macb_01", "Apple MacBook Pro 14", "Laptop", "Apple", "creative", 2400, false, 2023, 90),
        ("t_macb_02", "Apple MacBook Air 13", "Laptop", "Apple", "portable", 1200, false, 2022, 80),
        ("t_razer_blackwidow_01", "Razer BlackWidow mechanical keyboard", "Keyboard", "Razer", "gaming", 180, true, 2024, 75),
        ("t_sony_wh_01", "Sony WH1000 audio headset", "Audio", "Sony", "noise-cancelling", 330, true, 2023, 74),
        ("u_apple_01", "Apple MacBook notebook", "Laptop", "Apple", "typo", 1800, false, 2021, 66),
        ("u_apple_02", "Apple MacBook portable", "Laptop", "Apple", "typo", 1700, false, 2021, 65),
        ("u_bose_01", "Bose QuietComfort audio", "Audio", "Bose", "typo", 290, true, 2023, 61),
        ("u_bose_02", "Bose QuietComfort headset", "Audio", "Bose", "typo", 310, true, 2023, 60),
        ("u_lenovo_01", "Lenovo ThinkPad durable laptop", "Laptop", "Lenovo", "typo", 1300, false, 2023, 64),
        ("u_lenovo_02", "Lenovo ThinkPad business laptop", "Laptop", "Lenovo", "typo", 1500, false, 2022, 63),
        ("u_logitech_01", "Logitech MX Master mouse", "Accessories", "Logitech", "typo", 99, true, 2024, 62),
        ("u_logitech_02", "Logitech MX Master ergonomic mouse", "Accessories", "Logitech", "typo", 119, true, 2024, 61),
        ("u_samsung_01", "Samsung Galaxy phone", "Phone", "Samsung", "typo", 999, true, 2024, 67),
        ("u_samsung_02", "Samsung Galaxy tablet", "Tablet", "Samsung", "typo", 899, true, 2023, 66),
    ])
}

#[rustfmt::skip]
fn multi_word_and_facet_docs() -> Vec<Value> {
    let mut docs = vec![wh_seed_doc(2)];
    docs.extend(product_docs(&[
        ("m_apple_prof_01", "apple professional laptop creative", "Laptop", "Apple", "creative", 2200, false, 2023, 88),
        ("m_apple_prof_02", "apple professional laptop creative workstation", "Laptop", "Apple", "creative", 2600, false, 2022, 87),
        ("m_business_01", "business ultrabook durable battery", "Laptop", "Lenovo", "business", 1450, false, 2023, 78),
        ("m_business_02", "business ultrabook durable battery travel", "Laptop", "Dell", "business", 1550, false, 2022, 77),
        ("m_creator_monitor_01", "creator monitor color accurate", "Monitor", "LG", "creator", 780, false, 2024, 72),
        ("m_creator_monitor_02", "creator monitor color accurate display", "Monitor", "Dell", "creator", 820, false, 2024, 71),
        ("m_gaming_keyboard_01", "gaming mechanical keyboard rgb", "Keyboard", "Razer", "gaming", 210, true, 2024, 79),
        ("m_gaming_keyboard_02", "gaming mechanical keyboard rgb compact", "Keyboard", "Logitech", "gaming", 190, true, 2023, 76),
        ("m_wireless_01", "wireless noise cancelling audio", "Audio", "Bose", "noise-cancelling", 320, true, 2023, 86),
        ("m_wireless_02", "wireless noise cancelling audio headset", "Audio", "Sony", "noise-cancelling", 350, true, 2023, 85),
    ]));
    docs.extend(facet_group_docs("wireless", &[
        ("f_wireless_01", "Sony wireless audio headphones", "Audio", "Sony", "noise-cancelling", 330, true, 2023, 84),
        ("f_wireless_02", "Bose wireless audio earbuds", "Audio", "Bose", "noise-cancelling", 250, true, 2023, 83),
        ("f_wireless_03", "Sony wireless speaker", "Audio", "Sony", "bluetooth", 180, true, 2024, 82),
        ("f_wireless_04", "Logitech wireless receiver", "Accessories", "Logitech", "bluetooth", 45, true, 2024, 81),
        ("f_wireless_05", "Apple wireless audio buds", "Audio", "Apple", "spatial", 199, true, 2024, 80),
        ("f_wireless_06", "Sony wireless audio adapter", "Accessories", "Sony", "bluetooth", 120, true, 2023, 79),
    ]));
    docs
}

fn filter_docs() -> Vec<Value> {
    std::iter::once(wh_seed_doc(3))
        .chain((1..=12).map(|idx| {
            product_doc(
                &format!("flt_laptop_{idx:02}"),
                "laptop filtered exactmatch",
                "Laptop",
                if idx % 2 == 0 { "Dell" } else { "Apple" },
                "filter",
                700 + idx as i64,
                true,
                2024,
                40 + idx as i64,
            )
        }))
        .chain(vec![
            product_doc(
                "flt_laptop_miss_stock",
                "laptop filtered exactmatch",
                "Laptop",
                "Apple",
                "filter",
                900,
                false,
                2024,
                30,
            ),
            product_doc(
                "flt_laptop_miss_year",
                "laptop filtered exactmatch",
                "Laptop",
                "Dell",
                "filter",
                900,
                true,
                2022,
                30,
            ),
            product_doc(
                "flt_laptop_miss_price",
                "laptop filtered exactmatch",
                "Laptop",
                "Dell",
                "filter",
                3000,
                true,
                2024,
                30,
            ),
        ])
        .collect()
}

fn pagination_docs() -> Vec<Value> {
    std::iter::once(wh_seed_doc(4))
        .chain((1..=9).map(|idx| {
            product_doc(
                &format!("page_{idx:02}"),
                "pager constant specimen",
                "Pager",
                "PagerBrand",
                "paging",
                100 + idx as i64,
                true,
                2024,
                1,
            )
        }))
        .collect()
}

fn exact_hit_docs() -> Vec<Value> {
    std::iter::once(wh_seed_doc(5))
        .chain((1..=13).map(|idx| {
            product_doc(
                &format!("exact_{idx:02}"),
                "exactprobe counted specimen",
                "Exact",
                "ExactBrand",
                "exact",
                200 + idx as i64,
                true,
                2024,
                1,
            )
        }))
        .collect()
}

fn geo_and_highlight_docs() -> Vec<Value> {
    let mut highlight = product_doc(
        "h_wireless_audio_01",
        "highlightprobe target",
        "Highlight",
        "HighlightBrand",
        "highlight",
        1,
        true,
        2024,
        1,
    );
    highlight
        .as_object_mut()
        .unwrap()
        .insert("highlightText".to_string(), json!("wireless audio field"));
    vec![
        wh_seed_doc(6),
        geo_doc("geo_01", 0.0, 0.0),
        geo_doc("geo_02", 0.0, 0.001),
        geo_doc("geo_03", 0.001, 0.0),
        geo_doc("geo_04", 0.0, 1.0),
        highlight,
    ]
}

fn custom_ranking_docs() -> Vec<Value> {
    let mut docs = vec![wh_seed_doc(7)];
    docs.extend(product_docs(&[
        (
            "rank_01",
            "tieprobe neutral",
            "Ranking",
            "RankBrand",
            "rank",
            100,
            true,
            2024,
            50,
        ),
        (
            "rank_02",
            "tieprobe neutral",
            "Ranking",
            "RankBrand",
            "rank",
            100,
            true,
            2024,
            50,
        ),
        (
            "rank_03",
            "tieprobe neutral",
            "Ranking",
            "RankBrand",
            "rank",
            100,
            true,
            2024,
            50,
        ),
    ]));
    docs
}

fn filler_docs() -> Vec<Value> {
    (1..=322).map(|idx| filler_doc(8, idx)).collect()
}

pub const TEXT_SPECS: &[QuerySpec] = &[
    QuerySpec {
        name: "MacB",
        query: "MacB",
        query_type: "prefixLast",
        expected_ids: &["t_macb_01", "t_macb_02", "u_apple_01", "u_apple_02"],
        expected_total: 4,
    },
    QuerySpec {
        name: "Dell XPS",
        query: "Dell XPS",
        query_type: "prefixNone",
        expected_ids: &["t_dell_xps_01", "t_dell_xps_02"],
        expected_total: 2,
    },
    QuerySpec {
        name: "Sony WH",
        query: "Sony WH",
        query_type: "prefixLast",
        expected_ids: &["t_sony_wh_01"],
        expected_total: 1,
    },
    QuerySpec {
        name: "Razer BlackWidow",
        query: "Razer BlackWidow",
        query_type: "prefixNone",
        expected_ids: &["t_razer_blackwidow_01"],
        expected_total: 1,
    },
];

pub const TYPO_SPECS: &[QuerySpec] = &[
    QuerySpec {
        name: "Aple MacBok",
        query: "Aple MacBok",
        query_type: "prefixNone",
        expected_ids: &["t_macb_01", "t_macb_02", "u_apple_01", "u_apple_02"],
        expected_total: 4,
    },
    QuerySpec {
        name: "Samsng Galaxi",
        query: "Samsng Galaxi",
        query_type: "prefixNone",
        expected_ids: &["u_samsung_01", "u_samsung_02"],
        expected_total: 2,
    },
    QuerySpec {
        name: "Lenvo ThinkPad",
        query: "Lenvo ThinkPad",
        query_type: "prefixNone",
        expected_ids: &["u_lenovo_01", "u_lenovo_02"],
        expected_total: 2,
    },
    QuerySpec {
        name: "Logitec MX Mster",
        query: "Logitec MX Mster",
        query_type: "prefixNone",
        expected_ids: &["u_logitech_01", "u_logitech_02"],
        expected_total: 2,
    },
    QuerySpec {
        name: "Bose QuiteComfort",
        query: "Bose QuiteComfort",
        query_type: "prefixNone",
        expected_ids: &["u_bose_01", "u_bose_02"],
        expected_total: 2,
    },
];

pub const MULTI_WORD_SPECS: &[QuerySpec] = &[
    QuerySpec {
        name: "apple professional laptop creative",
        query: "apple professional laptop creative",
        query_type: "prefixNone",
        expected_ids: &["m_apple_prof_01", "m_apple_prof_02"],
        expected_total: 2,
    },
    QuerySpec {
        name: "wireless noise cancelling audio",
        query: "wireless noise cancelling audio",
        query_type: "prefixNone",
        expected_ids: &["m_wireless_01", "m_wireless_02"],
        expected_total: 2,
    },
    QuerySpec {
        name: "gaming mechanical keyboard rgb",
        query: "gaming mechanical keyboard rgb",
        query_type: "prefixNone",
        expected_ids: &["m_gaming_keyboard_01", "m_gaming_keyboard_02"],
        expected_total: 2,
    },
    QuerySpec {
        name: "business ultrabook durable battery",
        query: "business ultrabook durable battery",
        query_type: "prefixNone",
        expected_ids: &["m_business_01", "m_business_02"],
        expected_total: 2,
    },
    QuerySpec {
        name: "creator monitor color accurate",
        query: "creator monitor color accurate",
        query_type: "prefixNone",
        expected_ids: &["m_creator_monitor_01", "m_creator_monitor_02"],
        expected_total: 2,
    },
];

pub const FACET_QUERY: QuerySpec = QuerySpec {
    name: "wireless facet group",
    query: "",
    query_type: "prefixNone",
    expected_ids: &[
        "f_wireless_01",
        "f_wireless_02",
        "f_wireless_03",
        "f_wireless_04",
        "f_wireless_05",
    ],
    expected_total: 6,
};

pub const FACET_EXPECTATIONS: &[FacetExpectation] = &[
    FacetExpectation {
        field: "category",
        values: &[("Audio", 4), ("Accessories", 2)],
    },
    FacetExpectation {
        field: "brand",
        values: &[("Sony", 3), ("Apple", 1), ("Bose", 1), ("Logitech", 1)],
    },
    FacetExpectation {
        field: "tags",
        values: &[("bluetooth", 3), ("noise-cancelling", 2), ("spatial", 1)],
    },
    FacetExpectation {
        field: "price",
        values: &[
            ("120", 1),
            ("180", 1),
            ("199", 1),
            ("250", 1),
            ("330", 1),
            ("45", 1),
        ],
    },
];

pub const FILTER_EXPECTED_IDS: &[&str] = &[
    "flt_laptop_01",
    "flt_laptop_02",
    "flt_laptop_03",
    "flt_laptop_04",
    "flt_laptop_05",
    "flt_laptop_06",
    "flt_laptop_07",
    "flt_laptop_08",
    "flt_laptop_09",
    "flt_laptop_10",
    "flt_laptop_11",
    "flt_laptop_12",
];
pub const FILTER_TOTAL: usize = 12;
pub const EXACT_NB_HITS_LIMIT: usize = 5;
pub const EXACT_NB_HITS_EXPECTED_IDS: &[&str] =
    &["exact_01", "exact_02", "exact_03", "exact_04", "exact_05"];

pub const PAGINATION_EXPECTED_PAGES: &[&[&str]] = &[
    &["page_01", "page_02", "page_03"],
    &["page_04", "page_05", "page_06"],
    &["page_07", "page_08", "page_09"],
];
pub const PAGINATION_TOTAL: usize = 9;

pub const GEO_QUERY: QuerySpec = QuerySpec {
    name: "geoanchor",
    query: "geoanchor",
    query_type: "prefixNone",
    expected_ids: &["geo_01", "geo_02", "geo_03", "geo_04"],
    expected_total: 4,
};
pub const GEO_FILTERED_DISTANCES: &[(&str, f64)] = &[
    ("geo_01", 0.0),
    ("geo_02", 111.19492664455875),
    ("geo_03", 111.19492664455875),
];

pub const HIGHLIGHT_QUERY_WORDS: &[&str] = &["wireless", "audio"];
pub const HIGHLIGHT_EXPECTATION: HighlightExpectation = HighlightExpectation {
    document_id: "h_wireless_audio_01",
    field: "highlightText",
    value: "<em>wireless</em> <em>audio</em> field",
};

pub const CUSTOM_RANKING_QUERY: QuerySpec = QuerySpec {
    name: "tieprobe",
    query: "tieprobe",
    query_type: "prefixNone",
    expected_ids: &["rank_01", "rank_02", "rank_03"],
    expected_total: 3,
};

pub(super) fn laptop_filter() -> Filter {
    Filter::And(vec![
        Filter::GreaterThanOrEqual {
            field: "price".to_string(),
            value: FieldValue::Integer(500),
        },
        Filter::LessThanOrEqual {
            field: "price".to_string(),
            value: FieldValue::Integer(2500),
        },
        Filter::Equals {
            field: "inStock".to_string(),
            value: FieldValue::Bool(true),
        },
        Filter::Equals {
            field: "releaseYear".to_string(),
            value: FieldValue::Integer(2024),
        },
    ])
}

pub(super) fn geoloc(document: &ScoredDocument) -> Option<(f64, f64)> {
    let FieldValue::Object(point) = document.document.fields.get("_geoloc")? else {
        return None;
    };
    let lat = point.get("lat")?.as_float()?;
    let lng = point.get("lng")?.as_float()?;
    Some((lat, lng))
}
