//! Generate realistic demo analytics data for onboarding.
//!
//! Writes Parquet files directly to the analytics directory,
//! producing 30 days of realistic search + click + conversion events.

use super::config::AnalyticsConfig;
use super::schema::{InsightEvent, SearchEvent};
use std::collections::BTreeMap;

#[path = "seed_queries.rs"]
mod seed_queries;
use seed_queries::{DEFAULT_QUERIES, MOVIE_QUERIES, PRODUCT_QUERIES};

/// Realistic country distribution with IP ranges and optional region (state).
/// Format: (country, ip_prefix, weight, region)
const GEO_DISTRIBUTION: &[(&str, &str, f64, Option<&str>)] = &[
    ("US", "72.21.198.", 0.08, Some("California")),
    ("US", "98.137.11.", 0.07, Some("New York")),
    ("US", "66.220.149.", 0.06, Some("Texas")),
    ("US", "64.233.160.", 0.05, Some("Washington")),
    ("US", "17.142.160.", 0.04, Some("Illinois")),
    ("US", "68.180.228.", 0.03, Some("Florida")),
    ("US", "204.15.20.", 0.03, Some("Massachusetts")),
    ("US", "199.16.156.", 0.02, Some("Georgia")),
    ("US", "23.235.44.", 0.02, Some("Virginia")),
    ("US", "76.74.255.", 0.02, Some("Colorado")),
    ("US", "208.80.152.", 0.01, Some("Oregon")),
    ("US", "104.244.42.", 0.01, Some("Pennsylvania")),
    ("US", "151.101.1.", 0.01, Some("Ohio")),
    ("GB", "51.15.42.", 0.10, None),
    ("DE", "46.114.5.", 0.08, None),
    ("FR", "91.198.174.", 0.07, None),
    ("CA", "99.226.18.", 0.05, None),
    ("AU", "103.4.16.", 0.04, None),
    ("NL", "185.15.58.", 0.03, None),
    ("JP", "210.171.226.", 0.03, None),
    ("BR", "177.71.128.", 0.03, None),
    ("IN", "103.21.244.", 0.03, None),
    ("ES", "88.27.18.", 0.02, None),
    ("IT", "93.62.142.", 0.02, None),
    ("SE", "62.20.124.", 0.02, None),
    ("MX", "189.203.18.", 0.01, None),
    ("KR", "121.78.168.", 0.01, None),
    ("SG", "103.6.84.", 0.01, None),
];

/// Filter attributes and values for generating realistic filter analytics.
/// Format: (attribute, value, weight)
const FILTER_PATTERNS: &[(&str, &str, f64)] = &[
    ("brand", "Apple", 0.15),
    ("brand", "Samsung", 0.12),
    ("brand", "Sony", 0.08),
    ("brand", "Dell", 0.06),
    ("brand", "Google", 0.05),
    ("category", "Electronics", 0.14),
    ("category", "Laptops", 0.10),
    ("category", "Phones", 0.09),
    ("category", "Audio", 0.07),
    ("category", "Tablets", 0.06),
    ("price_range", "0-50", 0.04),
    ("price_range", "50-200", 0.02),
    ("price_range", "200-500", 0.02),
];

/// Device distribution tags.
const DEVICE_TAGS: &[(&str, f64)] = &[
    ("platform:desktop", 0.58),
    ("platform:mobile", 0.32),
    ("platform:tablet", 0.10),
];

/// Simple deterministic pseudo-random number generator (xorshift32).
/// Avoids pulling in the `rand` crate.
struct Rng {
    state: u32,
}

impl Rng {
    fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// Returns a value in [0.0, 1.0).
    fn next_f64(&mut self) -> f64 {
        (self.next_u32() as f64) / ((u32::MAX as f64) + 1.0)
    }

    /// Returns a value in [lo, hi].
    fn range(&mut self, lo: u32, hi: u32) -> u32 {
        if lo >= hi {
            return lo;
        }
        lo + (self.next_u32() % (hi - lo + 1))
    }

    /// Pick an index based on weighted distribution.
    fn weighted_pick(&mut self, weights: &[f64]) -> usize {
        let r = self.next_f64();
        let mut cumulative = 0.0;
        for (i, &w) in weights.iter().enumerate() {
            cumulative += w;
            if r < cumulative {
                return i;
            }
        }
        weights.len() - 1
    }
}

fn generate_query_id(rng: &mut Rng) -> String {
    let mut hex = String::with_capacity(32);
    for _ in 0..8 {
        let v = rng.next_u32();
        hex.push_str(&format!("{:08x}", v));
    }
    hex.truncate(32);
    hex
}

/// Pick the query set based on the index name.
fn queries_for_index(index_name: &str) -> &'static [(&'static str, u32, bool)] {
    let lower = index_name.to_lowercase();
    if lower.contains("movie") || lower.contains("film") || lower.contains("tmdb") {
        MOVIE_QUERIES
    } else if lower.contains("product")
        || lower.contains("bestbuy")
        || lower.contains("shop")
        || lower.contains("ecommerce")
        || lower.contains("commerce")
    {
        PRODUCT_QUERIES
    } else {
        DEFAULT_QUERIES
    }
}

/// Generate user tokens.
fn generate_users(rng: &mut Rng, count: usize) -> Vec<String> {
    (0..count)
        .map(|_| format!("user-{:08x}", rng.next_u32()))
        .collect()
}

/// Generate object IDs for click targets.
fn generate_object_ids(rng: &mut Rng, count: usize) -> Vec<String> {
    (0..count)
        .map(|_| format!("obj-{:06x}", rng.next_u32() % 0xffffff))
        .collect()
}

/// Result of seeding analytics data.
pub struct SeedResult {
    pub days: u32,
    pub seeded_dates: Vec<String>,
    pub total_searches: usize,
    pub total_clicks: usize,
    pub total_conversions: usize,
}

/// Optional controls for deterministic analytics fixtures.
#[derive(Debug, Clone)]
pub struct AnalyticsSeedOptions {
    pub days: u32,
    pub search_count: Option<u32>,
    pub no_result_rate: Option<f64>,
    pub device_distribution: Option<BTreeMap<String, f64>>,
    pub country_distribution: Option<BTreeMap<String, f64>>,
}

impl AnalyticsSeedOptions {
    pub fn for_days(days: u32) -> Self {
        Self {
            days,
            search_count: None,
            no_result_rate: None,
            device_distribution: None,
            country_distribution: None,
        }
    }
}

struct WeightedIndices {
    indices: Vec<usize>,
    weights: Vec<f64>,
}

impl WeightedIndices {
    fn new(indices: Vec<usize>, base_weights: &[f64]) -> Self {
        let sum: f64 = indices.iter().map(|index| base_weights[*index]).sum();
        let weights = indices
            .iter()
            .map(|index| base_weights[*index] / sum)
            .collect();
        Self { indices, weights }
    }

    fn pick(&self, rng: &mut Rng) -> usize {
        self.indices[rng.weighted_pick(&self.weights)]
    }
}

struct QueryPools {
    all: WeightedIndices,
    with_results: WeightedIndices,
    without_results: WeightedIndices,
}

impl QueryPools {
    fn new(queries: &[(&str, u32, bool)]) -> Self {
        let base_weights: Vec<f64> = queries
            .iter()
            .enumerate()
            .map(|(index, _)| 1.0 / ((index as f64) + 1.0).powf(0.8))
            .collect();
        let matching = |has_results| {
            queries
                .iter()
                .enumerate()
                .filter_map(|(index, query)| (query.2 == has_results).then_some(index))
                .collect()
        };
        Self {
            all: WeightedIndices::new((0..queries.len()).collect(), &base_weights),
            with_results: WeightedIndices::new(matching(true), &base_weights),
            without_results: WeightedIndices::new(matching(false), &base_weights),
        }
    }

    fn pick(&self, rng: &mut Rng, must_have_results: Option<bool>) -> usize {
        match must_have_results {
            Some(true) => self.with_results.pick(rng),
            Some(false) => self.without_results.pick(rng),
            None => self.all.pick(rng),
        }
    }
}

struct DeviceChoice {
    tag: String,
    weight: f64,
}

struct GeoChoice {
    country: String,
    ip_prefix: String,
    region: Option<String>,
    weight: f64,
}

struct SeedRuntime<'a> {
    index_name: &'a str,
    queries: &'static [(&'static str, u32, bool)],
    query_pools: QueryPools,
    users: Vec<String>,
    object_ids: Vec<String>,
    devices: Vec<DeviceChoice>,
    device_weights: Vec<f64>,
    geography: Vec<GeoChoice>,
    geo_weights: Vec<f64>,
    total_searches: u32,
    target_no_results: Option<u32>,
}

struct DayEvents {
    searches: Vec<SearchEvent>,
    insights: Vec<InsightEvent>,
}

/// Validate caller-controlled seed options before any filesystem mutation.
pub fn validate_seed_options(options: &AnalyticsSeedOptions) -> Result<(), String> {
    if options.days == 0 || options.days > 90 {
        return Err("days must be between 1 and 90".to_string());
    }
    if matches!(options.search_count, Some(0 | 100_001..)) {
        return Err("searchCount must be between 1 and 100000".to_string());
    }
    if let Some(rate) = options.no_result_rate {
        if !rate.is_finite() || !(0.0..=1.0).contains(&rate) {
            return Err("noResultRate must be between 0 and 1".to_string());
        }
    }
    if let Some(distribution) = &options.device_distribution {
        validate_distribution("deviceDistribution", distribution)?;
        if distribution
            .keys()
            .any(|device| !matches!(device.as_str(), "desktop" | "mobile" | "tablet"))
        {
            return Err("deviceDistribution supports desktop, mobile, and tablet".to_string());
        }
    }
    if let Some(distribution) = &options.country_distribution {
        validate_distribution("countryDistribution", distribution)?;
        if distribution.keys().any(|country| {
            country.len() != 2 || !country.bytes().all(|byte| byte.is_ascii_uppercase())
        }) {
            return Err("countryDistribution keys must be ISO alpha-2 uppercase codes".to_string());
        }
    }
    Ok(())
}

fn validate_distribution(name: &str, distribution: &BTreeMap<String, f64>) -> Result<(), String> {
    if distribution.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if distribution
        .values()
        .any(|weight| !weight.is_finite() || *weight < 0.0)
    {
        return Err(format!("{name} weights must be finite and non-negative"));
    }
    let total: f64 = distribution.values().sum();
    if (total - 1.0).abs() > 0.000_001 {
        return Err(format!("{name} weights must sum to 1"));
    }
    Ok(())
}

fn resolve_devices(distribution: Option<&BTreeMap<String, f64>>) -> Vec<DeviceChoice> {
    match distribution {
        Some(configured) => configured
            .iter()
            .map(|(device, weight)| DeviceChoice {
                tag: format!("platform:{device}"),
                weight: *weight,
            })
            .collect(),
        None => DEVICE_TAGS
            .iter()
            .map(|(tag, weight)| DeviceChoice {
                tag: (*tag).to_string(),
                weight: *weight,
            })
            .collect(),
    }
}

fn resolve_geography(distribution: Option<&BTreeMap<String, f64>>) -> Vec<GeoChoice> {
    match distribution {
        Some(configured) => configured
            .iter()
            .flat_map(|(country, weight)| expand_country_regions(country, *weight))
            .collect(),
        None => GEO_DISTRIBUTION
            .iter()
            .map(|(country, prefix, weight, region)| GeoChoice {
                country: (*country).to_string(),
                ip_prefix: (*prefix).to_string(),
                region: region.map(str::to_string),
                weight: *weight,
            })
            .collect(),
    }
}

/// Spread one configured country weight across every `GEO_DISTRIBUTION` row for that country.
///
/// `GEO_DISTRIBUTION` holds one row per (country, region), so a country such as `US` owns 13
/// region rows. Collapsing a configured country to a single row would emit one region for the
/// whole country and erase the region breakdown that `/2/geo/{country}/regions` reports.
fn expand_country_regions(country: &str, weight: f64) -> Vec<GeoChoice> {
    let references: Vec<_> = GEO_DISTRIBUTION
        .iter()
        .filter(|entry| entry.0 == country)
        .collect();
    let reference_weight_total: f64 = references.iter().map(|entry| entry.2).sum();
    if references.is_empty() || reference_weight_total <= 0.0 {
        return vec![GeoChoice {
            country: country.to_string(),
            ip_prefix: "203.0.113.".to_string(),
            region: None,
            weight,
        }];
    }
    references
        .iter()
        .map(|(_, prefix, reference_weight, region)| GeoChoice {
            country: country.to_string(),
            ip_prefix: (*prefix).to_string(),
            region: region.map(str::to_string),
            weight: weight * reference_weight / reference_weight_total,
        })
        .collect()
}

fn daily_search_counts(
    options: &AnalyticsSeedOptions,
    now: chrono::DateTime<chrono::Utc>,
    rng: &mut Rng,
) -> Vec<u32> {
    if let Some(total) = options.search_count {
        let per_day = total / options.days;
        let remainder = total % options.days;
        return (0..options.days)
            .map(|day| per_day + u32::from(day < remainder))
            .collect();
    }

    (1..=options.days)
        .rev()
        .map(|day_offset| {
            let date = now - chrono::Duration::days(day_offset as i64);
            let weekend_factor = if date.format("%u").to_string().parse::<u32>().unwrap_or(1) >= 6 {
                0.6
            } else {
                1.0
            };
            (800.0 * weekend_factor * (0.8 + rng.next_f64() * 0.4)) as u32
        })
        .collect()
}

fn must_have_results(runtime: &SeedRuntime<'_>, ordinal: u32) -> Option<bool> {
    runtime.target_no_results.map(|target| {
        let total = u64::from(runtime.total_searches);
        let target = u64::from(target);
        let current = u64::from(ordinal);
        let is_no_result = ((current + 1) * target / total) > (current * target / total);
        !is_no_result
    })
}

fn generate_search_event(
    runtime: &SeedRuntime<'_>,
    rng: &mut Rng,
    timestamp_ms: i64,
    ordinal: u32,
) -> SearchEvent {
    let query_index = runtime
        .query_pools
        .pick(rng, must_have_results(runtime, ordinal));
    let (query_text, approx_hits, has_results) = runtime.queries[query_index];
    let nb_hits = if has_results {
        (approx_hits as f64 * (0.7 + rng.next_f64() * 0.6)).max(1.0) as u32
    } else {
        0
    };
    let user_idx = rng.range(0, runtime.users.len() as u32 - 1) as usize;
    let device = &runtime.devices[rng.weighted_pick(&runtime.device_weights)];
    let geography = &runtime.geography[rng.weighted_pick(&runtime.geo_weights)];
    let filter = generate_filter(rng, has_results);

    SearchEvent {
        timestamp_ms,
        query: query_text.to_string(),
        query_id: Some(generate_query_id(rng)),
        index_name: runtime.index_name.to_string(),
        nb_hits,
        processing_time_ms: rng.range(2, 45),
        user_token: Some(runtime.users[user_idx].clone()),
        user_ip: Some(format!("{}{}", geography.ip_prefix, rng.range(1, 254))),
        filters: filter,
        facets: None,
        analytics_tags: Some(format!("{},source:organic", device.tag)),
        page: 0,
        hits_per_page: 20,
        has_results,
        country: Some(geography.country.clone()),
        region: geography.region.clone(),
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    }
}

fn generate_filter(rng: &mut Rng, has_results: bool) -> Option<String> {
    if !has_results || rng.next_f64() >= 0.30 {
        return None;
    }
    let weights: Vec<f64> = FILTER_PATTERNS
        .iter()
        .map(|(_, _, weight)| *weight)
        .collect();
    let (attribute, value, _) = FILTER_PATTERNS[rng.weighted_pick(&weights)];
    Some(format!("{attribute}:{value}"))
}

fn generate_insights(
    runtime: &SeedRuntime<'_>,
    rng: &mut Rng,
    search: &SearchEvent,
) -> Vec<InsightEvent> {
    if !search.has_results || rng.next_f64() >= 0.35 {
        return Vec::new();
    }
    let object_index = rng.range(0, runtime.object_ids.len() as u32 - 1) as usize;
    let object_id = runtime.object_ids[object_index].clone();
    let query_id = search.query_id.clone();
    let user_token = search.user_token.clone().unwrap_or_default();
    let mut events = vec![InsightEvent {
        event_type: "click".to_string(),
        event_subtype: None,
        event_name: "Result Clicked".to_string(),
        index: runtime.index_name.to_string(),
        user_token: user_token.clone(),
        authenticated_user_token: None,
        query_id: query_id.clone(),
        object_ids: vec![object_id.clone()],
        object_ids_alt: vec![],
        positions: Some(vec![generate_click_position(rng)]),
        timestamp: Some(search.timestamp_ms + rng.range(500, 5000) as i64),
        value: None,
        currency: None,
        interleaving_team: None,
    }];
    if rng.next_f64() < 0.15 {
        events.push(InsightEvent {
            event_type: "conversion".to_string(),
            event_subtype: None,
            event_name: "Product Purchased".to_string(),
            index: runtime.index_name.to_string(),
            user_token,
            authenticated_user_token: None,
            query_id,
            object_ids: vec![object_id],
            object_ids_alt: vec![],
            positions: None,
            timestamp: Some(search.timestamp_ms + rng.range(10_000, 120_000) as i64),
            value: Some((rng.range(500, 15000) as f64) / 100.0),
            currency: Some("USD".to_string()),
            interleaving_team: None,
        });
    }
    events
}

fn generate_day(
    runtime: &SeedRuntime<'_>,
    rng: &mut Rng,
    day_start_ms: i64,
    search_count: u32,
    ordinal_start: u32,
) -> DayEvents {
    let mut searches = Vec::with_capacity(search_count as usize);
    let mut insights = Vec::new();
    for day_ordinal in 0..search_count {
        let timestamp_ms = day_start_ms + generate_time_of_day_ms(rng);
        let search = generate_search_event(runtime, rng, timestamp_ms, ordinal_start + day_ordinal);
        insights.extend(generate_insights(runtime, rng, &search));
        searches.push(search);
    }
    DayEvents { searches, insights }
}

fn write_day(
    config: &AnalyticsConfig,
    index_name: &str,
    date: chrono::DateTime<chrono::Utc>,
    events: &DayEvents,
) -> Result<(), String> {
    let date_str = date.format("%Y-%m-%d");
    let search_partition = config
        .searches_dir(index_name)
        .join(format!("date={date_str}"));
    std::fs::create_dir_all(&search_partition)
        .map_err(|error| format!("Failed to create search partition dir: {error}"))?;
    write_search_events_to_partition(&events.searches, &search_partition)?;

    if !events.insights.is_empty() {
        let insight_partition = config
            .events_dir(index_name)
            .join(format!("date={date_str}"));
        std::fs::create_dir_all(&insight_partition)
            .map_err(|error| format!("Failed to create events partition dir: {error}"))?;
        write_insight_events_to_partition(&events.insights, &insight_partition)?;
    }
    Ok(())
}

/// Seed analytics data for the given index.
///
/// Generates `days` days of realistic data (default 30) written directly
/// to Parquet files in the analytics directory.
pub fn seed_analytics(
    config: &AnalyticsConfig,
    index_name: &str,
    days: u32,
) -> Result<SeedResult, String> {
    seed_analytics_with_options(config, index_name, &AnalyticsSeedOptions::for_days(days))
}

/// Seed analytics using deterministic volume and distribution controls when supplied.
pub fn seed_analytics_with_options(
    config: &AnalyticsConfig,
    index_name: &str,
    options: &AnalyticsSeedOptions,
) -> Result<SeedResult, String> {
    validate_seed_options(options)?;
    super::mutation::with_index_mutation(config, index_name, || {
        seed_analytics_exclusively(config, index_name, options)
    })
}

fn seed_analytics_exclusively(
    config: &AnalyticsConfig,
    index_name: &str,
    options: &AnalyticsSeedOptions,
) -> Result<SeedResult, String> {
    let queries = queries_for_index(index_name);
    let mut rng = Rng::new(
        index_name
            .bytes()
            .fold(42u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32)),
    );

    let now = chrono::Utc::now();
    let daily_counts = daily_search_counts(options, now, &mut rng);
    let total_searches: u32 = daily_counts.iter().sum();
    let target_no_results = options
        .no_result_rate
        .map(|rate| (f64::from(total_searches) * rate).round() as u32);
    let devices = resolve_devices(options.device_distribution.as_ref());
    let device_weights = devices.iter().map(|choice| choice.weight).collect();
    let geography = resolve_geography(options.country_distribution.as_ref());
    let geo_weights = geography.iter().map(|choice| choice.weight).collect();
    let runtime = SeedRuntime {
        index_name,
        queries,
        query_pools: QueryPools::new(queries),
        users: generate_users(&mut rng, 350),
        object_ids: generate_object_ids(&mut rng, 200),
        devices,
        device_weights,
        geography,
        geo_weights,
        total_searches,
        target_no_results,
    };
    let mut ordinal = 0;
    let mut seeded_dates = Vec::with_capacity(options.days as usize);
    let mut total_clicks = 0;
    let mut total_conversions = 0;

    for (day_index, search_count) in daily_counts.into_iter().enumerate() {
        let day_offset = options.days - day_index as u32;
        let date = now - chrono::Duration::days(day_offset as i64);
        seeded_dates.push(date.format("%Y-%m-%d").to_string());
        let day_start_ms = date
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();

        let day_events = generate_day(&runtime, &mut rng, day_start_ms, search_count, ordinal);
        total_clicks += day_events
            .insights
            .iter()
            .filter(|event| event.event_type == "click")
            .count();
        total_conversions += day_events
            .insights
            .iter()
            .filter(|event| event.event_type == "conversion")
            .count();
        write_day(config, index_name, date, &day_events)?;
        ordinal += search_count;
    }

    Ok(SeedResult {
        days: options.days,
        seeded_dates,
        total_searches: total_searches as usize,
        total_clicks,
        total_conversions,
    })
}

/// Clear search and insight analytics for one index without racing a seed.
pub fn clear_analytics(config: &AnalyticsConfig, index_name: &str) -> Result<u64, String> {
    super::mutation::clear_index(config, index_name)
}

/// Generate a realistic time-of-day offset in milliseconds.
/// Traffic peaks around 10am-2pm and 7pm-10pm, low overnight.
fn generate_time_of_day_ms(rng: &mut Rng) -> i64 {
    // Hour distribution weights (0-23)
    let hour_weights: [f64; 24] = [
        0.01, 0.005, 0.003, 0.003, 0.005, 0.01, // 0-5am: very low
        0.02, 0.04, 0.06, 0.08, 0.09, 0.09, // 6-11am: ramp up
        0.08, 0.07, 0.06, 0.05, 0.05, 0.06, // 12-5pm: afternoon
        0.07, 0.08, 0.07, 0.05, 0.03, 0.02, // 6-11pm: evening peak then drop
    ];

    let hour = rng.weighted_pick(&hour_weights) as i64;
    let minute = rng.range(0, 59) as i64;
    let second = rng.range(0, 59) as i64;
    let ms = rng.range(0, 999) as i64;

    (hour * 3600 + minute * 60 + second) * 1000 + ms
}

/// Generate a realistic click position (1-indexed).
/// ~40% pos 1, ~20% pos 2, ~15% pos 3, tapering off.
fn generate_click_position(rng: &mut Rng) -> u32 {
    let weights = [
        0.40, 0.20, 0.12, 0.08, 0.06, 0.04, 0.03, 0.02, 0.02, 0.01, 0.01, 0.01,
    ];
    (rng.weighted_pick(&weights) + 1) as u32
}

/// Write search events to a specific date partition.
fn write_search_events_to_partition(
    events: &[SearchEvent],
    partition_dir: &std::path::Path,
) -> Result<(), String> {
    let schema = super::schema::search_event_schema();
    let batch = super::writer::search_events_to_batch(events, &schema)?;
    let path = partition_dir.join("seed_searches.parquet");
    super::writer::write_parquet_file_atomic(&path, batch)
}

/// Write insight events to a specific date partition.
fn write_insight_events_to_partition(
    events: &[InsightEvent],
    partition_dir: &std::path::Path,
) -> Result<(), String> {
    let schema = super::schema::insight_event_schema();
    let batch = super::writer::insight_events_to_batch(events, &schema)?;
    let path = partition_dir.join("seed_events.parquet");
    super::writer::write_parquet_file_atomic(&path, batch)
}

#[cfg(test)]
#[path = "seed_tests.rs"]
mod tests;
