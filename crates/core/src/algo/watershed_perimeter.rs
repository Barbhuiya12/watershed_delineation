//! perimeter : MultiPolygon → PerimeterKm   (geodesic, WGS84, exterior rings)
//!
//! Companion to [`crate::algo::watershed_area`]. Both measure the same
//! assembled watershed on the WGS84 ellipsoid with Karney's algorithm, so the
//! shape indices built from the pair (Gravelius compactness, circularity,
//! form factor, elongation) never mix a geodesic area with a projected length.

use std::fmt;

use geo::{GeodesicArea, MultiPolygon, Polygon};
use tracing::{debug, info, instrument};

/// Conversion factor from metres to kilometres.
const M_TO_KM: f64 = 1e-3;

/// Perimeter measured in kilometres.
///
/// A separate newtype from [`crate::algo::AreaKm2`] so a boundary length and
/// an area cannot be swapped at a call site. Does NOT derive `Eq`/`Ord`
/// because `f64` does not support them soundly.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct PerimeterKm(f64);

impl PerimeterKm {
    /// Create a new perimeter value.
    pub fn new(km: f64) -> Self {
        Self(km)
    }

    /// Return the raw `f64` value.
    pub fn as_f64(self) -> f64 {
        self.0
    }
}

impl fmt::Display for PerimeterKm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} km", self.0)
    }
}

/// Errors from geodesic perimeter computation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WatershedPerimeterError {
    /// Returned when the input multi-polygon contains no polygons.
    #[error("cannot compute perimeter of empty geometry")]
    EmptyGeometry,

    /// Returned when the geodesic perimeter sum yields a non-finite value.
    #[error("geodesic perimeter returned non-finite value: {raw_m} m")]
    NonFinitePerimeter {
        /// The raw perimeter in metres that was non-finite.
        raw_m: f64,
    },
}

/// Compute the geodesic perimeter of a polygon's exterior ring on WGS84.
///
/// Interior rings are excluded: the watershed boundary that morphometric
/// indices refer to is the outer edge, and counting hole rings would inflate
/// every compactness ratio computed from the result.
///
/// # Errors
///
/// | Condition | Error |
/// |-----------|-------|
/// | Result is non-finite | [`WatershedPerimeterError::NonFinitePerimeter`] |
#[instrument(skip(polygon))]
pub fn geodesic_perimeter(polygon: &Polygon<f64>) -> Result<PerimeterKm, WatershedPerimeterError> {
    let perimeter_m = exterior_perimeter_m(polygon);

    if !perimeter_m.is_finite() {
        return Err(WatershedPerimeterError::NonFinitePerimeter { raw_m: perimeter_m });
    }

    let perimeter_km = perimeter_m * M_TO_KM;
    debug!(perimeter_km, "geodesic polygon perimeter computed");
    Ok(PerimeterKm::new(perimeter_km))
}

/// Compute the geodesic perimeter of a multi-polygon on WGS84.
///
/// Sums the exterior ring of every constituent polygon. A multi-part
/// watershed therefore reports the total boundary of all its parts.
///
/// # Errors
///
/// | Condition | Error |
/// |-----------|-------|
/// | Multi-polygon has no polygons | [`WatershedPerimeterError::EmptyGeometry`] |
/// | Result is non-finite | [`WatershedPerimeterError::NonFinitePerimeter`] |
#[instrument(skip(multi_polygon))]
pub fn geodesic_perimeter_multi(
    multi_polygon: &MultiPolygon<f64>,
) -> Result<PerimeterKm, WatershedPerimeterError> {
    if multi_polygon.0.is_empty() {
        return Err(WatershedPerimeterError::EmptyGeometry);
    }

    let perimeter_m: f64 = multi_polygon.iter().map(exterior_perimeter_m).sum();

    if !perimeter_m.is_finite() {
        return Err(WatershedPerimeterError::NonFinitePerimeter { raw_m: perimeter_m });
    }

    let perimeter_km = perimeter_m * M_TO_KM;
    info!(
        perimeter_km,
        polygon_count = multi_polygon.0.len(),
        "geodesic multi-polygon perimeter computed"
    );
    Ok(PerimeterKm::new(perimeter_km))
}

/// Geodesic length of a polygon's exterior ring, in metres.
///
// ponytail: clones the exterior ring into a hole-free `Polygon` because
// `GeodesicArea::geodesic_perimeter` measures interior rings too. Swap for a
// direct ring-length call if ring cloning ever shows up in a profile.
fn exterior_perimeter_m(polygon: &Polygon<f64>) -> f64 {
    Polygon::new(polygon.exterior().clone(), Vec::new()).geodesic_perimeter()
}

#[cfg(test)]
mod tests {
    use geo::{LineString, MultiPolygon, Polygon};

    use super::{
        PerimeterKm, WatershedPerimeterError, geodesic_perimeter, geodesic_perimeter_multi,
    };

    /// Build a geographic rectangle polygon from southwest to northeast corners.
    fn geo_rect(west: f64, south: f64, east: f64, north: f64) -> Polygon<f64> {
        Polygon::new(ring(west, south, east, north), vec![])
    }

    fn ring(west: f64, south: f64, east: f64, north: f64) -> LineString<f64> {
        LineString::from(vec![
            (west, south),
            (east, south),
            (east, north),
            (west, north),
            (west, south),
        ])
    }

    #[test]
    fn one_degree_square_at_equator() {
        // 111.32 km (equator) + 111.30 km (1°N) + 2 × 110.57 km (meridian arcs).
        let result = geodesic_perimeter(&geo_rect(0.0, 0.0, 1.0, 1.0))
            .unwrap()
            .as_f64();
        let expected = 443.8_f64;
        let rel_err = (result - expected).abs() / expected;
        assert!(
            rel_err < 0.01,
            "perimeter {result:.1} km deviates {:.2}% from expected {expected} km",
            rel_err * 100.0
        );
    }

    #[test]
    fn holes_do_not_count_towards_perimeter() {
        let solid = geo_rect(0.0, 0.0, 1.0, 1.0);
        let holed = Polygon::new(ring(0.0, 0.0, 1.0, 1.0), vec![ring(0.25, 0.25, 0.75, 0.75)]);
        assert_eq!(
            geodesic_perimeter(&solid).unwrap(),
            geodesic_perimeter(&holed).unwrap(),
            "an interior ring must not change the outer boundary length"
        );
    }

    #[test]
    fn multi_polygon_sums_its_parts() {
        let a = geo_rect(0.0, 0.0, 1.0, 1.0);
        let b = geo_rect(10.0, 0.0, 11.0, 1.0);
        let single =
            geodesic_perimeter(&a).unwrap().as_f64() + geodesic_perimeter(&b).unwrap().as_f64();
        let multi = geodesic_perimeter_multi(&MultiPolygon::new(vec![a, b]))
            .unwrap()
            .as_f64();
        assert!((multi - single).abs() < 1e-6, "{multi} != {single}");
    }

    #[test]
    fn empty_multi_polygon_is_an_error() {
        assert_eq!(
            geodesic_perimeter_multi(&MultiPolygon::new(vec![])),
            Err(WatershedPerimeterError::EmptyGeometry)
        );
    }

    #[test]
    fn perimeter_km_display() {
        assert_eq!(format!("{}", PerimeterKm::new(12.5)), "12.5 km");
    }
}
