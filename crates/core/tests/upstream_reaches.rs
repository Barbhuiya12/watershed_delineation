//! Channel-network materialization: bbox sweep narrowed by delineated membership.
//!
//! The risk this file guards is specific. `Engine::upstream_reaches` queries the
//! snap auxiliary by the watershed's bounding rectangle, which is a coarse
//! filter: linework belonging to units *outside* the watershed can sit inside
//! that same rectangle. If the `unit_id` narrowing were dropped, drainage
//! density would silently inflate.

use hfx::StemRole;
use pourpoint_core::algo::coord::GeoCoord;
use pourpoint_core::session::DatasetSession;
use pourpoint_core::testutil::{DatasetBuilder, TestCatchment, TestSnapGeometry, TestSnapTarget};
use pourpoint_core::{DelineationOptions, Engine, LevelSelection, RefinementMode};

/// Three units in an L so the delineated pair's bounding rectangle swallows the
/// third — which is downstream, and so must not contribute any channel length.
///
/// ```text
///   lat 2 ┌───────┐
///         │  10   │          union bbox of {10, 20} = (0,0)-(2,2)
///   lat 1 ├───────┼───────┐  unit 30 sits inside it, and is NOT a member
///         │  30   │  20   │
///   lat 0 └───────┴───────┘
///        lon 0   lon 1   lon 2
/// ```
///
/// Chain built by the fixture: 10 → 20 → 30. Delineating at unit 20 gives the
/// upstream set {20, 10}; unit 30 is downstream.
fn l_shaped_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let catchments = vec![
        TestCatchment {
            id: 10,
            area_km2: 100.0,
            up_area_km2: Some(100.0),
            polygon: (0.0, 1.0, 1.0, 2.0),
        },
        TestCatchment {
            id: 20,
            area_km2: 100.0,
            up_area_km2: Some(200.0),
            polygon: (1.0, 0.0, 2.0, 1.0),
        },
        TestCatchment {
            id: 30,
            area_km2: 100.0,
            up_area_km2: Some(300.0),
            polygon: (0.0, 0.0, 1.0, 1.0),
        },
    ];

    let targets = vec![
        TestSnapTarget {
            id: 1,
            catchment_id: 10,
            weight: 100.0,
            is_mainstem: true,
            geometry: TestSnapGeometry::LineString(0.2, 1.2, 0.8, 1.8),
        },
        TestSnapTarget {
            id: 2,
            catchment_id: 20,
            weight: 200.0,
            is_mainstem: true,
            geometry: TestSnapGeometry::LineString(1.2, 0.2, 1.8, 0.8),
        },
        // Downstream neighbour: inside the query rectangle, outside the basin.
        TestSnapTarget {
            id: 3,
            catchment_id: 30,
            weight: 300.0,
            is_mainstem: true,
            geometry: TestSnapGeometry::LineString(0.2, 0.2, 0.8, 0.8),
        },
    ];

    DatasetBuilder::new(3)
        .with_custom_catchments(catchments)
        .with_custom_snap_targets(targets)
        .build()
}

/// Delineate at unit 20 and return the staged pieces needed for reach queries.
fn delineate_at_unit_20(root: &std::path::Path) -> (Engine, pourpoint_core::PreMergeDrainageUnits) {
    let session = DatasetSession::open_path(root).expect("fixture should open");
    let engine = Engine::builder(session).build();
    let options = DelineationOptions::default().with_refinement_mode(RefinementMode::Disabled);

    let level = engine
        .select_level(LevelSelection::Finest)
        .expect("finest level");
    let resolved = engine
        .resolve_outlet_at_level(GeoCoord::new(1.5, 0.5), level, options.resolver_config())
        .expect("outlet on unit 20 linework should resolve");
    let upstream = engine
        .traverse_upstream_at_level(&resolved)
        .expect("traversal");
    let units = engine
        .produce_pre_merge_units(&upstream)
        .expect("pre-merge units");
    (engine, units)
}

#[test]
fn reaches_exclude_units_outside_the_delineated_set() {
    let (_dir, root) = l_shaped_fixture();
    let (engine, units) = delineate_at_unit_20(&root);

    let members: Vec<i64> = units.units().iter().map(|unit| unit.id().get()).collect();
    assert_eq!(members.len(), 2, "expected {{20, 10}}, got {members:?}");

    let reaches = engine
        .upstream_reaches(&units)
        .expect("reach query should succeed");

    let mut reach_units: Vec<i64> = reaches
        .reaches()
        .iter()
        .map(|reach| reach.unit_id().get())
        .collect();
    reach_units.sort_unstable();

    assert_eq!(
        reach_units,
        vec![10, 20],
        "unit 30's reach sits inside the query rectangle but is downstream; \
         it must be filtered out by unit_id, not counted"
    );
}

#[test]
fn total_length_sums_exactly_the_retained_reaches() {
    let (_dir, root) = l_shaped_fixture();
    let (engine, units) = delineate_at_unit_20(&root);
    let reaches = engine.upstream_reaches(&units).expect("reach query");

    let summed: f64 = reaches
        .reaches()
        .iter()
        .map(|reach| reach.length().as_f64())
        .sum();

    assert!(
        (reaches.total_length().as_f64() - summed).abs() < 1e-9,
        "total_length {} must equal the sum of its parts {summed}",
        reaches.total_length().as_f64()
    );
    assert!(
        reaches.total_length().as_f64() > 0.0,
        "two diagonal degree-scale reaches must carry real length"
    );
}

#[test]
fn every_reach_carries_length_role_and_original_wkb() {
    let (_dir, root) = l_shaped_fixture();
    let (engine, units) = delineate_at_unit_20(&root);
    let reaches = engine.upstream_reaches(&units).expect("reach query");

    for reach in reaches.reaches() {
        assert!(
            reach.length().as_f64() > 0.0,
            "reach {} measured zero",
            reach.snap_id().get()
        );
        assert_eq!(
            reach.stem_role(),
            Some(StemRole::Mainstem),
            "fixture declares every target as mainstem"
        );
        assert!(
            !reach.geometry_wkb().is_empty(),
            "the fabric's own WKB bytes must be carried through"
        );
    }

    assert_eq!(
        reaches.count_of(StemRole::Mainstem),
        reaches.len(),
        "mainstem count must cover every retained reach"
    );
    assert_eq!(
        reaches.count_of(StemRole::Distributary),
        0,
        "a dendritic fixture declares no distributary"
    );
}

/// A single-unit basin whose only reach runs corner to corner along the
/// bounding box.
///
/// This pins the degenerate cases of the query rectangle: one unit rather than
/// many, and linework lying exactly on the edge rather than inside it — the
/// case where an f64-to-f32 narrowing error would show up if there were one.
#[test]
fn a_single_unit_basin_retains_its_boundary_reach() {
    let (_dir, root) = DatasetBuilder::new(1)
        .with_custom_catchments(vec![TestCatchment {
            id: 10,
            area_km2: 100.0,
            up_area_km2: Some(100.0),
            polygon: (0.0, 0.0, 1.0, 1.0),
        }])
        .with_custom_snap_targets(vec![TestSnapTarget {
            id: 1,
            catchment_id: 10,
            weight: 100.0,
            is_mainstem: true,
            // Corner to corner: every vertex lies on the bbox boundary.
            geometry: TestSnapGeometry::LineString(0.0, 0.0, 1.0, 1.0),
        }])
        .build();

    let session = DatasetSession::open_path(&root).expect("fixture should open");
    let engine = Engine::builder(session).build();
    let options = DelineationOptions::default().with_refinement_mode(RefinementMode::Disabled);
    let level = engine.select_level(LevelSelection::Finest).expect("level");
    let resolved = engine
        .resolve_outlet_at_level(GeoCoord::new(0.5, 0.5), level, options.resolver_config())
        .expect("outlet on the diagonal should resolve");
    let upstream = engine
        .traverse_upstream_at_level(&resolved)
        .expect("traversal");
    let units = engine.produce_pre_merge_units(&upstream).expect("units");

    let reaches = engine
        .upstream_reaches(&units)
        .expect("a single-unit basin must still produce a valid query rectangle");

    assert_eq!(
        reaches.len(),
        1,
        "a corner-to-corner reach lies on the query rectangle and must be retained"
    );
    assert!(
        reaches.total_length().as_f64() > 100.0,
        "a one-degree diagonal is ~157 km, got {}",
        reaches.total_length().as_f64()
    );
}

/// A dataset with no snap declaration still delineates, but asking it for a
/// channel network is a caller error — not a silently empty answer that would
/// read as "this basin has no rivers".
#[test]
fn reach_query_reports_a_dataset_that_declares_no_snap_auxiliary() {
    let (_dir, root) = DatasetBuilder::new(3).build();
    let session = DatasetSession::open_path(&root).expect("fixture should open");
    let engine = Engine::builder(session).build();
    let options = DelineationOptions::default().with_refinement_mode(RefinementMode::Disabled);

    let level = engine.select_level(LevelSelection::Finest).expect("level");
    let center = DatasetBuilder::new(3)
        .generated_terminal_unit_center()
        .expect("generated fixture exposes its terminal centre");
    let resolved = engine
        .resolve_outlet_at_level(center, level, options.resolver_config())
        .expect("containment resolution needs no snap file");
    let upstream = engine
        .traverse_upstream_at_level(&resolved)
        .expect("traversal");
    let units = engine.produce_pre_merge_units(&upstream).expect("units");

    let err = engine
        .upstream_reaches(&units)
        .expect_err("no snap auxiliary means no channel network to return");
    assert!(
        matches!(err, pourpoint_core::EngineError::NoSnapAuxForLevel { .. }),
        "expected NoSnapAuxForLevel, got {err:?}"
    );
}

/// A Point stem inside the basin is reported rather than counted as zero
/// length, which would understate drainage density without saying so.
#[test]
fn a_point_stem_is_reported_rather_than_counted_as_zero() {
    let (_dir, root) = DatasetBuilder::new(1)
        .with_custom_catchments(vec![TestCatchment {
            id: 10,
            area_km2: 100.0,
            up_area_km2: Some(100.0),
            polygon: (0.0, 0.0, 1.0, 1.0),
        }])
        .with_custom_snap_targets(vec![TestSnapTarget {
            id: 1,
            catchment_id: 10,
            weight: 100.0,
            is_mainstem: true,
            geometry: TestSnapGeometry::Point(0.5, 0.5),
        }])
        .build();

    let session = DatasetSession::open_path(&root).expect("fixture should open");
    let engine = Engine::builder(session).build();
    let options = DelineationOptions::default().with_refinement_mode(RefinementMode::Disabled);
    let level = engine.select_level(LevelSelection::Finest).expect("level");
    let resolved = engine
        .resolve_outlet_at_level(GeoCoord::new(0.5, 0.5), level, options.resolver_config())
        .expect("point target should resolve");
    let upstream = engine
        .traverse_upstream_at_level(&resolved)
        .expect("traversal");
    let units = engine.produce_pre_merge_units(&upstream).expect("units");

    let err = engine
        .upstream_reaches(&units)
        .expect_err("a Point carries no length and must not be silently counted as 0 km");
    assert!(
        matches!(err, pourpoint_core::EngineError::ReachLength { .. }),
        "expected ReachLength, got {err:?}"
    );
}
