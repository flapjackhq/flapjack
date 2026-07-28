use crate::types::FieldValue;

const EARTH_RADIUS_M: f64 = 6_371_000.0;

pub fn haversine(lat1: f64, lng1: f64, lat2: f64, lng2: f64) -> f64 {
    let dlat = (lat2 - lat1).to_radians();
    let dlng = (lng2 - lng1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlng / 2.0).sin().powi(2);
    EARTH_RADIUS_M * 2.0 * a.sqrt().asin()
}

pub fn point_in_box(
    lat: f64,
    lng: f64,
    p1_lat: f64,
    p1_lng: f64,
    p2_lat: f64,
    p2_lng: f64,
) -> bool {
    let min_lat = p1_lat.min(p2_lat);
    let max_lat = p1_lat.max(p2_lat);
    let min_lng = p1_lng.min(p2_lng);
    let max_lng = p1_lng.max(p2_lng);
    lat >= min_lat && lat <= max_lat && lng >= min_lng && lng <= max_lng
}

/// Determine if a point lies inside a polygon using the ray casting algorithm.
///
/// # Arguments
///
/// * `lat` - The latitude coordinate to test.
/// * `lng` - The longitude coordinate to test.
/// * `polygon` - A slice of (latitude, longitude) tuples forming a closed polygon.
///
/// # Returns
///
/// True if the point is inside the polygon, false otherwise. Returns false if the polygon has fewer than 3 vertices.
pub fn point_in_polygon(lat: f64, lng: f64, polygon: &[(f64, f64)]) -> bool {
    let n = polygon.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (yi, xi) = polygon[i];
        let (yj, xj) = polygon[j];
        if ((yi > lat) != (yj > lat)) && (lng < (xj - xi) * (lat - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[derive(Debug, Clone)]
pub struct GeoPoint {
    pub lat: f64,
    pub lng: f64,
}

#[derive(Debug, Clone)]
pub struct BoundingBox {
    pub p1_lat: f64,
    pub p1_lng: f64,
    pub p2_lat: f64,
    pub p2_lng: f64,
}

#[derive(Debug, Clone)]
pub struct AroundPrecisionRange {
    pub from: u64,
    pub value: u64,
}

#[derive(Debug, Clone, Default)]
pub struct AroundPrecisionConfig {
    pub ranges: Vec<AroundPrecisionRange>,
    pub fixed: Option<u64>,
}

impl AroundPrecisionConfig {
    /// Quantize a raw distance into a bucket using either a fixed precision or
    /// the configured range thresholds (falling back to 10m minimum).
    pub fn bucket_distance(&self, distance_m: f64) -> u64 {
        let dist = distance_m as u64;
        if let Some(fixed) = self.fixed {
            let precision = fixed.max(10);
            return dist / precision;
        }
        if !self.ranges.is_empty() {
            let mut precision = 10u64;
            for r in &self.ranges {
                if dist >= r.from {
                    precision = r.value.max(1);
                } else {
                    break;
                }
            }
            return dist / precision;
        }
        dist
    }
}

#[derive(Debug, Clone)]
pub struct GeoParams {
    pub around: Option<GeoPoint>,
    pub around_radius: Option<AroundRadius>,
    pub bounding_boxes: Vec<BoundingBox>,
    pub polygons: Vec<Vec<(f64, f64)>>,
    pub around_precision: AroundPrecisionConfig,
    pub minimum_around_radius: Option<u64>,
}

#[derive(Debug, Clone)]
pub enum AroundRadius {
    Meters(u64),
    All,
}

impl GeoParams {
    pub fn is_empty(&self) -> bool {
        self.around.is_none() && self.bounding_boxes.is_empty() && self.polygons.is_empty()
    }

    pub fn has_around(&self) -> bool {
        self.around.is_some()
    }

    pub fn has_geo_filter(&self) -> bool {
        !self.bounding_boxes.is_empty() || !self.polygons.is_empty() || self.around.is_some()
    }

    /// Test whether a (lat, lng) point passes the configured geo constraints:
    /// bounding-box containment, polygon containment, or within-radius of the
    /// `aroundLatLng` center.
    pub fn filter_point(&self, lat: f64, lng: f64) -> bool {
        if !self.bounding_boxes.is_empty() {
            return self
                .bounding_boxes
                .iter()
                .any(|bb| point_in_box(lat, lng, bb.p1_lat, bb.p1_lng, bb.p2_lat, bb.p2_lng));
        }

        if !self.polygons.is_empty() {
            return self
                .polygons
                .iter()
                .any(|poly| point_in_polygon(lat, lng, poly));
        }

        if let Some(ref center) = self.around {
            let dist = haversine(center.lat, center.lng, lat, lng);
            return match &self.around_radius {
                Some(AroundRadius::Meters(r)) => dist <= *r as f64,
                Some(AroundRadius::All) => true,
                None => true,
            };
        }

        true
    }

    pub fn distance_from_center(&self, lat: f64, lng: f64) -> Option<f64> {
        self.around
            .as_ref()
            .map(|c| haversine(c.lat, c.lng, lat, lng))
    }
}

pub fn extract_single_geoloc(value: &FieldValue) -> Option<(f64, f64)> {
    match value {
        FieldValue::Object(map) => {
            let lat = match map.get("lat")? {
                FieldValue::Float(f) => *f,
                FieldValue::Integer(i) => *i as f64,
                _ => return None,
            };
            let lng = match map.get("lng")? {
                FieldValue::Float(f) => *f,
                FieldValue::Integer(i) => *i as f64,
                _ => return None,
            };
            Some((lat, lng))
        }
        _ => None,
    }
}

pub fn extract_all_geolocs(geoloc: Option<&FieldValue>) -> Vec<(f64, f64)> {
    match geoloc {
        Some(FieldValue::Object(_)) => geoloc.and_then(extract_single_geoloc).into_iter().collect(),
        Some(FieldValue::Array(arr)) => arr.iter().filter_map(extract_single_geoloc).collect(),
        _ => Vec::new(),
    }
}

pub fn best_geoloc_for_filter(points: &[(f64, f64)], geo_params: &GeoParams) -> Option<(f64, f64)> {
    if points.is_empty() {
        return None;
    }
    if let Some(ref center) = geo_params.around {
        points
            .iter()
            .filter(|(lat, lng)| geo_params.filter_point(*lat, *lng))
            .min_by(|(lat_a, lng_a), (lat_b, lng_b)| {
                let da = haversine(center.lat, center.lng, *lat_a, *lng_a);
                let db = haversine(center.lat, center.lng, *lat_b, *lng_b);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
    } else {
        points
            .iter()
            .find(|(lat, lng)| geo_params.filter_point(*lat, *lng))
            .copied()
    }
}

pub fn parse_around_lat_lng(s: &str) -> Option<GeoPoint> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 2 {
        return None;
    }
    let lat = parts[0].trim().parse::<f64>().ok()?;
    let lng = parts[1].trim().parse::<f64>().ok()?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
        return None;
    }
    Some(GeoPoint { lat, lng })
}

pub fn parse_around_radius(v: &serde_json::Value) -> Option<AroundRadius> {
    match v {
        serde_json::Value::String(s) if s == "all" => Some(AroundRadius::All),
        serde_json::Value::Number(n) => n.as_u64().map(AroundRadius::Meters),
        _ => None,
    }
}

/// Parse JSON into a vector of bounding boxes, each defined by four coordinates: [p1_lat, p1_lng, p2_lat, p2_lng].
///
/// Accepts nested arrays `[[lat, lng, lat, lng]]`, flat arrays `[lat, lng, lat, lng, ...]`, or comma-separated strings `"lat,lng,lat,lng,..."`.
///
/// # Arguments
///
/// * `v` - A JSON value to parse.
///
/// # Returns
///
/// A vector of parsed bounding boxes. Returns an empty vector if input is invalid or coordinates are incomplete.
pub fn parse_bounding_boxes(v: &serde_json::Value) -> Vec<BoundingBox> {
    let mut boxes = Vec::new();
    match v {
        serde_json::Value::Array(outer) => {
            if outer.is_empty() {
                return boxes;
            }
            if outer[0].is_array() {
                for inner in outer {
                    if let serde_json::Value::Array(coords) = inner {
                        if coords.len() == 4 {
                            if let (Some(a), Some(b), Some(c), Some(d)) = (
                                coords[0].as_f64(),
                                coords[1].as_f64(),
                                coords[2].as_f64(),
                                coords[3].as_f64(),
                            ) {
                                boxes.push(BoundingBox {
                                    p1_lat: a,
                                    p1_lng: b,
                                    p2_lat: c,
                                    p2_lng: d,
                                });
                            }
                        }
                    }
                }
            } else {
                let floats: Vec<f64> = outer.iter().filter_map(|v| v.as_f64()).collect();
                for chunk in floats.chunks(4) {
                    if chunk.len() == 4 {
                        boxes.push(BoundingBox {
                            p1_lat: chunk[0],
                            p1_lng: chunk[1],
                            p2_lat: chunk[2],
                            p2_lng: chunk[3],
                        });
                    }
                }
            }
        }
        serde_json::Value::String(s) => {
            let floats: Vec<f64> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            for chunk in floats.chunks(4) {
                if chunk.len() == 4 {
                    boxes.push(BoundingBox {
                        p1_lat: chunk[0],
                        p1_lng: chunk[1],
                        p2_lat: chunk[2],
                        p2_lng: chunk[3],
                    });
                }
            }
        }
        _ => {}
    }
    boxes
}

/// Parse JSON into a vector of polygons, where each polygon is a sequence of (latitude, longitude) coordinate pairs.
///
/// Accepts nested arrays `[[lat, lng, lat, lng, ...]]`, flat arrays `[lat, lng, lat, lng, ...]`, or comma-separated strings `"lat,lng,lat,lng,..."`.
///
/// # Arguments
///
/// * `v` - A JSON value to parse.
///
/// # Returns
///
/// A vector of polygons; each polygon is a vector of (lat, lng) tuples. Returns an empty vector if input is invalid or has fewer than 6 coordinates (less than 3 points).
pub fn parse_polygons(v: &serde_json::Value) -> Vec<Vec<(f64, f64)>> {
    let mut polygons = Vec::new();
    match v {
        serde_json::Value::Array(outer) => {
            if outer.is_empty() {
                return polygons;
            }
            if outer[0].is_array() {
                for inner in outer {
                    if let serde_json::Value::Array(coords) = inner {
                        let floats: Vec<f64> = coords.iter().filter_map(|v| v.as_f64()).collect();
                        if floats.len() >= 6 && floats.len().is_multiple_of(2) {
                            let pts: Vec<(f64, f64)> =
                                floats.chunks(2).map(|c| (c[0], c[1])).collect();
                            polygons.push(pts);
                        }
                    }
                }
            } else {
                let floats: Vec<f64> = outer.iter().filter_map(|v| v.as_f64()).collect();
                if floats.len() >= 6 && floats.len().is_multiple_of(2) {
                    let pts: Vec<(f64, f64)> = floats.chunks(2).map(|c| (c[0], c[1])).collect();
                    polygons.push(pts);
                }
            }
        }
        serde_json::Value::String(s) => {
            let floats: Vec<f64> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            if floats.len() >= 6 && floats.len().is_multiple_of(2) {
                let pts: Vec<(f64, f64)> = floats.chunks(2).map(|c| (c[0], c[1])).collect();
                polygons.push(pts);
            }
        }
        _ => {}
    }
    polygons
}
/// Parse JSON into an AroundPrecisionConfig for precision-based distance bucketing.
///
/// Accepts a single fixed precision number or an array of objects with `from` (distance threshold) and `value` (precision). Ranges are automatically sorted by distance.
///
/// # Arguments
///
/// * `v` - A JSON value to parse.
///
/// # Returns
///
/// An AroundPrecisionConfig with either fixed precision or distance-based ranges. Returns default empty config if input is invalid.
pub fn parse_around_precision(v: &serde_json::Value) -> AroundPrecisionConfig {
    match v {
        serde_json::Value::Number(n) => AroundPrecisionConfig {
            fixed: n.as_u64(),
            ranges: vec![],
        },
        serde_json::Value::Array(arr) => {
            let mut ranges: Vec<AroundPrecisionRange> = arr
                .iter()
                .filter_map(|item| {
                    let from = item.get("from")?.as_u64()?;
                    let value = item.get("value")?.as_u64()?;
                    Some(AroundPrecisionRange { from, value })
                })
                .collect();
            ranges.sort_by_key(|r| r.from);
            AroundPrecisionConfig {
                fixed: None,
                ranges,
            }
        }
        _ => AroundPrecisionConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_haversine_nyc_to_la() {
        let d = haversine(40.7128, -74.0060, 34.0522, -118.2437);
        assert!(
            (d - 3_944_000.0).abs() < 10_000.0,
            "NYC to LA should be ~3944km, got {}",
            d
        );
    }

    #[test]
    fn test_haversine_same_point() {
        let d = haversine(40.7128, -74.0060, 40.7128, -74.0060);
        assert!(d < 0.01, "Same point distance should be ~0");
    }

    #[test]
    fn test_point_in_box() {
        assert!(point_in_box(40.71, -74.00, 40.0, -75.0, 41.0, -73.0));
        assert!(!point_in_box(35.0, -74.00, 40.0, -75.0, 41.0, -73.0));
    }

    #[test]
    fn test_point_in_polygon_triangle() {
        let triangle = vec![(0.0, 0.0), (0.0, 10.0), (10.0, 0.0)];
        assert!(point_in_polygon(2.0, 2.0, &triangle));
        assert!(!point_in_polygon(8.0, 8.0, &triangle));
    }

    #[test]
    fn test_parse_around_lat_lng() {
        let p = parse_around_lat_lng("40.71, -74.01").unwrap();
        assert!((p.lat - 40.71).abs() < 0.001);
        assert!((p.lng - (-74.01)).abs() < 0.001);
        assert!(parse_around_lat_lng("invalid").is_none());
        assert!(parse_around_lat_lng("91.0, 0.0").is_none());
    }

    #[test]
    fn test_parse_bounding_boxes_nested() {
        let v = serde_json::json!([[47.3165, 4.9665, 47.3424, 5.0201]]);
        let boxes = parse_bounding_boxes(&v);
        assert_eq!(boxes.len(), 1);
        assert!((boxes[0].p1_lat - 47.3165).abs() < 0.001);
    }

    #[test]
    fn test_parse_bounding_boxes_flat() {
        let v = serde_json::json!([47.3165, 4.9665, 47.3424, 5.0201]);
        let boxes = parse_bounding_boxes(&v);
        assert_eq!(boxes.len(), 1);
    }

    #[test]
    fn test_parse_polygons() {
        let v = serde_json::json!([[47.3165, 4.9665, 47.3424, 5.0201, 47.32, 4.98]]);
        let polys = parse_polygons(&v);
        assert_eq!(polys.len(), 1);
        assert_eq!(polys[0].len(), 3);
    }

    /// Verify that filter_point correctly accepts points inside a bounding box and rejects points outside it.
    #[test]
    fn test_geo_params_filter_bounding_box() {
        let params = GeoParams {
            around: None,
            around_radius: None,
            bounding_boxes: vec![BoundingBox {
                p1_lat: 40.0,
                p1_lng: -75.0,
                p2_lat: 41.0,
                p2_lng: -73.0,
            }],
            polygons: vec![],
            around_precision: AroundPrecisionConfig::default(),
            minimum_around_radius: None,
        };
        assert!(params.filter_point(40.5, -74.0));
        assert!(!params.filter_point(35.0, -74.0));
    }

    #[test]
    fn test_geo_params_filter_around() {
        let params = GeoParams {
            around: Some(GeoPoint {
                lat: 40.7128,
                lng: -74.0060,
            }),
            around_radius: Some(AroundRadius::Meters(10_000)),
            bounding_boxes: vec![],
            polygons: vec![],
            around_precision: AroundPrecisionConfig::default(),
            minimum_around_radius: None,
        };
        assert!(params.filter_point(40.72, -74.00));
        assert!(!params.filter_point(41.5, -74.00));
    }

    #[test]
    fn test_geo_params_around_all() {
        let params = GeoParams {
            around: Some(GeoPoint { lat: 0.0, lng: 0.0 }),
            around_radius: Some(AroundRadius::All),
            bounding_boxes: vec![],
            polygons: vec![],
            around_precision: AroundPrecisionConfig::default(),
            minimum_around_radius: None,
        };
        assert!(params.filter_point(89.0, 179.0));
    }

    /// Verify that bounding box filters take precedence over around-radius filters when both are configured.
    #[test]
    fn test_bbox_wins_over_around() {
        let params = GeoParams {
            around: Some(GeoPoint {
                lat: 40.7128,
                lng: -74.0060,
            }),
            around_radius: Some(AroundRadius::Meters(100)),
            bounding_boxes: vec![BoundingBox {
                p1_lat: 30.0,
                p1_lng: -80.0,
                p2_lat: 50.0,
                p2_lng: -70.0,
            }],
            polygons: vec![],
            around_precision: AroundPrecisionConfig::default(),
            minimum_around_radius: None,
        };
        assert!(params.filter_point(35.0, -75.0));
    }

    #[test]
    fn filter_point_doc_mentions_polygon_constraints() {
        let source = include_str!("geo.rs");
        let before_fn = source
            .split("pub fn filter_point(&self, lat: f64, lng: f64) -> bool")
            .next()
            .unwrap_or_default();
        let doc_window = before_fn
            .lines()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            doc_window.contains("polygon"),
            "GeoParams::filter_point docs must mention polygon-based filtering"
        );
    }
}
