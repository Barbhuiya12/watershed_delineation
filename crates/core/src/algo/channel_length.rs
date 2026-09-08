//! channelLength : Geometry → ChannelLengthKm   (geodesic, WGS84)
//!
//! The numerator of drainage density. Reach centrelines arrive from the HFX
//! snap auxiliary as WKB linework in EPSG:4326; measuring them on the WGS84
//! ellipsoid — rather than in a projection chosen per basin — keeps the length
//! commensurate with the geodesic area from [`crate::algo::watershed_area`],
//! so `ΣL / A` is a ratio of two quantities measured the same way.

use std::fmt;

use geo::{Geodesic, Geometry, Length};
use tracing::instrument;

/// Conversion factor from metres to kilometres.
const M_TO_KM: f64 = 1e-3;

/// Channel length measured in kilometres.
///
/// Distinct from [`crate::algo::PerimeterKm`] because the two are never
/// interchangeable: swapping a basin perimeter for a channel length silently
/// turns drainage density into a shape index. Does NOT derive `Eq`/`Ord`
/// because `f64` does not support them soundly.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct ChannelLengthKm(f64);

impl ChannelLengthKm {
    /// A zero-length channel, the identity for summation.
    pub const ZERO: Self = Self(0.0);

    /// Create a new channel length value.
    pub fn new(km: f64) -> Self {
        Self(km)
    }

    /// Return the raw `f64` value.
    pub fn as_f64(self) -> f64 {
        self.0
    }
}

impl std::ops::Add for ChannelLengthKm {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl std::iter::Sum for ChannelLengthKm {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, std::ops::Add::add)
    }
}

impl fmt::Display for ChannelLengthKm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} km", self.0)
    }
}

/// Errors from geodesic channel-length computation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ChannelLengthError {
    /// Fired when a snap geometry is not linear.
    ///
    /// The HFX snap auxiliary may declare Point stems as well as linework. A
    /// point carries no length, and treating it as zero would understate
    /// drainage density without saying so — so it is reported instead.
    #[error("expected linear geometry for channel length, found {geometry_type}")]
    NotLinear {
        /// The geometry variant that was supplied.
        geometry_type: &'static str,
    },

    /// Fired when the geodesic length is not finite.
    #[error("geodesic channel length returned non-finite value: {raw_m} m")]
    NonFiniteLength {
        /// The raw length in metres that was non-finite.
        raw_m: f64,
    },
}

/// Compute the geodesic length of a reach centreline on WGS84.
///
/// Accepts `LineString` and `MultiLineString`; every other geometry variant is
/// an error rather than a silent zero.
///
/// # Errors
///
/// | Condition | Error |
/// |-----------|-------|
/// | Geometry is not linear | [`ChannelLengthError::NotLinear`] |
/// | Result is non-finite | [`ChannelLengthError::NonFiniteLength`] |
#[instrument(skip(geometry))]
pub fn geodesic_channel_length(
    geometry: &Geometry<f64>,
) -> Result<ChannelLengthKm, ChannelLengthError> {
    let length_m = match geometry {
        Geometry::LineString(line) => line.length::<Geodesic>(),
        Geometry::MultiLineString(lines) => lines.length::<Geodesic>(),
        other => {
            return Err(ChannelLengthError::NotLinear {
                geometry_type: geometry_variant_name(other),
            });
        }
    };

    if !length_m.is_finite() {
        return Err(ChannelLengthError::NonFiniteLength { raw_m: length_m });
    }

    Ok(ChannelLengthKm::new(length_m * M_TO_KM))
}

/// Name the geometry variant for error messages.
fn geometry_variant_name(geometry: &Geometry<f64>) -> &'static str {
    match geometry {
        Geometry::Point(_) => "Point",
        Geometry::Line(_) => "Line",
        Geometry::LineString(_) => "LineString",
        Geometry::Polygon(_) => "Polygon",
        Geometry::MultiPoint(_) => "MultiPoint",
        Geometry::MultiLineString(_) => "MultiLineString",
        Geometry::MultiPolygon(_) => "MultiPolygon",
        Geometry::GeometryCollection(_) => "GeometryCollection",
        Geometry::Rect(_) => "Rect",
        Geometry::Triangle(_) => "Triangle",
    }
}

#[cfg(test)]
mod tests {
    use geo::{Geometry, LineString, MultiLineString, Point, line_string};

    use super::{ChannelLengthError, ChannelLengthKm, geodesic_channel_length};

    /// One degree of longitude along the equator ≈ 111.32 km.
    #[test]
    fn one_degree_along_the_equator() {
        let line: LineString<f64> = line_string![(x: 0.0, y: 0.0), (x: 1.0, y: 0.0)];
        let km = geodesic_channel_length(&Geometry::LineString(line))
            .unwrap()
            .as_f64();
        let rel_err = (km - 111.32).abs() / 111.32;
        assert!(rel_err < 0.01, "expected ≈111.32 km, got {km:.3} km");
    }

    #[test]
    fn multi_line_string_sums_its_parts() {
        let a: LineString<f64> = line_string![(x: 0.0, y: 0.0), (x: 1.0, y: 0.0)];
        let b: LineString<f64> = line_string![(x: 10.0, y: 0.0), (x: 11.0, y: 0.0)];
        let one = geodesic_channel_length(&Geometry::LineString(a.clone()))
            .unwrap()
            .as_f64();
        let multi =
            geodesic_channel_length(&Geometry::MultiLineString(MultiLineString::new(vec![a, b])))
                .unwrap()
                .as_f64();
        assert!(
            (multi - 2.0 * one).abs() < 1e-6,
            "two equal equatorial degrees should double: {multi} vs {one}"
        );
    }

    #[test]
    fn vertices_are_followed_not_short_circuited() {
        // A dog-leg must measure longer than the straight line between its ends.
        let bent: LineString<f64> =
            line_string![(x: 0.0, y: 0.0), (x: 0.5, y: 0.5), (x: 1.0, y: 0.0)];
        let straight: LineString<f64> = line_string![(x: 0.0, y: 0.0), (x: 1.0, y: 0.0)];
        let bent_km = geodesic_channel_length(&Geometry::LineString(bent))
            .unwrap()
            .as_f64();
        let straight_km = geodesic_channel_length(&Geometry::LineString(straight))
            .unwrap()
            .as_f64();
        assert!(
            bent_km > straight_km,
            "sinuous reach {bent_km:.2} km must exceed straight {straight_km:.2} km"
        );
    }

    #[test]
    fn a_point_stem_is_an_error_not_a_zero() {
        assert_eq!(
            geodesic_channel_length(&Geometry::Point(Point::new(0.0, 0.0))),
            Err(ChannelLengthError::NotLinear {
                geometry_type: "Point"
            })
        );
    }

    #[test]
    fn zero_is_the_summation_identity() {
        assert_eq!(
            (ChannelLengthKm::ZERO + ChannelLengthKm::new(5.0)).as_f64(),
            5.0
        );
        let summed: ChannelLengthKm = [ChannelLengthKm::new(1.5), ChannelLengthKm::new(2.5)]
            .into_iter()
            .sum();
        assert_eq!(summed.as_f64(), 4.0);
    }
}
