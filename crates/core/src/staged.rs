//! stagedDelineation : GeoCoord → OutletResolution → UpstreamUnits → Watershed
//!
//! This module names the intermediate values that the staged engine path passes
//! between independently callable `Engine` methods.
//!
//! ```rust,ignore
//! pub fn select_level(&self, choice: LevelSelection) -> Result<SelectedLevel, EngineError>;
//!
//! pub fn resolve_outlet_at_level(
//!     &self,
//!     outlet: GeoCoord,
//!     level: SelectedLevel,
//!     config: &ResolverConfig,
//! ) -> Result<LevelResolvedOutlet, EngineError>;
//!
//! pub fn traverse_upstream_at_level(
//!     &self,
//!     outlet: &LevelResolvedOutlet,
//! ) -> Result<SameLevelUpstreamUnits, EngineError>;
//!
//! pub fn produce_pre_merge_units(
//!     &self,
//!     upstream: &SameLevelUpstreamUnits,
//! ) -> Result<PreMergeDrainageUnits, EngineError>;
//!
//! pub fn refine_terminal(
//!     &self,
//!     resolved: &LevelResolvedOutlet,
//!     units: &PreMergeDrainageUnits,
//!     options: &DelineationOptions,
//! ) -> Result<TerminalRefinement, EngineError>;
//!
//! pub fn dissolve_watershed(
//!     &self,
//!     units: &PreMergeDrainageUnits,
//!     refinement: &TerminalRefinement,
//!     options: &DelineationOptions,
//! ) -> Result<DissolvedWatershed, EngineError>;
//!
//! pub fn compose_result(
//!     &self,
//!     resolved: LevelResolvedOutlet,
//!     upstream: SameLevelUpstreamUnits,
//!     units: &PreMergeDrainageUnits,
//!     refinement: TerminalRefinement,
//!     dissolved: DissolvedWatershed,
//! ) -> DelineationResult;
//! ```
//!
//! Stage order:
//!
//! ```mermaid
//! flowchart LR
//!     select[select level]
//!     resolve[resolve outlet within level]
//!     traverse[traverse upstream same-level graph]
//!     records[produce pre-merge drainage-unit records]
//!     refine[terminal refinement strategy seam]
//!     dissolve[dissolve/assemble]
//!     compose[compose result]
//!
//!     select --> resolve --> traverse --> records --> refine --> dissolve --> compose
//! ```

use geo::{Geometry, MultiPolygon};
use hfx::{Level, OutletCoord, SnapId, StemRole, UnitId, Weight};

use crate::algo::coord::GeoCoord;
use crate::algo::{AreaKm2, ChannelLengthKm, UpstreamUnits};
use crate::refinement::{
    AppliedRefinementProvenance, BestEffortRefinementProvenance, BestEffortSkipReason,
    ContainedTerminalPolygon, RefinementStrategyName,
};
use crate::resolver::OutletResolution;
#[allow(deprecated)]
use crate::resolver::ResolvedOutlet;

/// Selects the HFX drainage-unit level used for the staged delineation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelSelection {
    /// Use the finest level present in the loaded dataset.
    Finest,
}

/// Dataset-proven selected drainage-unit level.
///
/// The wrapped [`Level`] is private so downstream stages cannot be called with
/// an arbitrary raw level. Construction goes through `Engine::select_level`
/// after consulting `DatasetSession`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedLevel {
    level: Level,
}

impl SelectedLevel {
    /// Construct a selected level after the dataset session proves it exists.
    pub(crate) fn from_proven_level(level: Level) -> Self {
        Self { level }
    }

    /// Construct a selected level for focused integration tests.
    #[cfg(feature = "test-fixtures")]
    pub fn from_proven_level_for_test(level: Level) -> Self {
        Self::from_proven_level(level)
    }

    /// Return the selected HFX level.
    pub fn level(self) -> Level {
        self.level
    }
}

/// Controls whether terminal refinement is attempted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum RefinementMode {
    /// Try raster refinement when the dataset and engine provide raster inputs.
    #[default]
    BestEffort,
    /// Require declared D8 raster refinement and fail if it cannot be applied.
    RequireD8,
    /// Skip terminal refinement and dissolve whole drainage-unit polygons.
    Disabled,
}

impl From<bool> for RefinementMode {
    fn from(refine: bool) -> Self {
        if refine {
            Self::BestEffort
        } else {
            Self::Disabled
        }
    }
}

/// Outlet resolution result constrained to the selected level.
///
/// The typed authority and deprecated legacy view are derived together during
/// construction. Both fields are private and only shared views are exposed, so
/// they cannot diverge after construction.
#[allow(deprecated)]
#[derive(Debug, Clone, PartialEq)]
pub struct LevelResolvedOutlet {
    selected_level: SelectedLevel,
    authority: OutletResolution,
    legacy_resolved: ResolvedOutlet,
}

#[allow(deprecated)]
impl LevelResolvedOutlet {
    /// Construct a level-resolved outlet after the resolver stage has constrained it.
    pub(crate) fn new(selected_level: SelectedLevel, authority: OutletResolution) -> Self {
        let legacy_resolved = authority.clone().into();
        Self {
            selected_level,
            authority,
            legacy_resolved,
        }
    }

    /// Return the selected level used during outlet resolution.
    pub fn selected_level(&self) -> SelectedLevel {
        self.selected_level
    }

    /// Return the legacy fielded outlet payload.
    #[deprecated(note = "use LevelResolvedOutlet::authority for typed outlet authority")]
    pub fn resolved(&self) -> &ResolvedOutlet {
        &self.legacy_resolved
    }

    /// Return the typed outlet authority chosen during resolution.
    pub fn authority(&self) -> &OutletResolution {
        &self.authority
    }
}

/// Same-level upstream traversal result for a level-resolved outlet.
#[derive(Debug, Clone, PartialEq)]
pub struct SameLevelUpstreamUnits {
    terminal: UnitId,
    selected_level: SelectedLevel,
    upstream: UpstreamUnits,
}

impl SameLevelUpstreamUnits {
    /// Construct same-level upstream units after traversal validates the level invariant.
    pub(crate) fn new(
        terminal: UnitId,
        selected_level: SelectedLevel,
        upstream: UpstreamUnits,
    ) -> Self {
        Self {
            terminal,
            selected_level,
            upstream,
        }
    }

    /// Return the terminal unit at the selected level.
    pub fn terminal(&self) -> UnitId {
        self.terminal
    }

    /// Return the selected level shared by every upstream unit.
    pub fn selected_level(&self) -> SelectedLevel {
        self.selected_level
    }

    /// Return the inclusive upstream unit set, terminal first.
    pub fn upstream(&self) -> &UpstreamUnits {
        &self.upstream
    }
}

/// Pristine drainage-unit record before terminal carving or dissolve.
///
/// This record intentionally exposes source drainage-unit data, not final
/// watershed output. Summing [`area`](Self::area) across pre-merge records does
/// not define final `area_km2`, and unioning these geometries does not define
/// final refined geometry. The final geometry and area are produced only by the
/// downstream dissolve stage.
#[derive(Debug, Clone, PartialEq)]
pub struct PreMergeDrainageUnit {
    id: UnitId,
    level: Level,
    area: hfx::AreaKm2,
    up_area: Option<hfx::AreaKm2>,
    outlet: OutletCoord,
    geometry: MultiPolygon<f64>,
}

impl PreMergeDrainageUnit {
    /// Construct a pristine pre-merge drainage-unit record from source fields.
    pub(crate) fn new(
        id: UnitId,
        level: Level,
        area: hfx::AreaKm2,
        up_area: Option<hfx::AreaKm2>,
        outlet: OutletCoord,
        geometry: MultiPolygon<f64>,
    ) -> Self {
        Self {
            id,
            level,
            area,
            up_area,
            outlet,
            geometry,
        }
    }

    /// Construct a pre-merge record for focused integration tests.
    #[cfg(feature = "test-fixtures")]
    pub fn new_for_test(
        id: UnitId,
        level: Level,
        area: hfx::AreaKm2,
        up_area: Option<hfx::AreaKm2>,
        outlet: OutletCoord,
        geometry: MultiPolygon<f64>,
    ) -> Self {
        Self::new(id, level, area, up_area, outlet, geometry)
    }

    /// Return the drainage unit ID.
    pub fn id(&self) -> UnitId {
        self.id
    }

    /// Return the HFX level of this drainage unit.
    pub fn level(&self) -> Level {
        self.level
    }

    /// Return the local drainage area from `catchments.parquet`.
    pub fn area(&self) -> hfx::AreaKm2 {
        self.area
    }

    /// Return the total upstream drainage area from `catchments.parquet`, if present.
    pub fn up_area(&self) -> Option<hfx::AreaKm2> {
        self.up_area
    }

    /// Return the declared outlet coordinate for this drainage unit.
    pub fn outlet(&self) -> OutletCoord {
        self.outlet
    }

    /// Return the whole drainage-unit geometry before terminal refinement.
    pub fn geometry(&self) -> &MultiPolygon<f64> {
        &self.geometry
    }
}

/// Terminal-first collection of pre-merge drainage-unit records.
///
/// Includes the whole terminal polygon and never a carved terminal. The
/// terminal-first ordering exists for typed inspection; it cannot affect final
/// geometry because the downstream dissolve path re-sorts polygons by spatial
/// key before reducing them.
#[derive(Debug, Clone, PartialEq)]
pub struct PreMergeDrainageUnits {
    terminal: UnitId,
    selected_level: SelectedLevel,
    units: Vec<PreMergeDrainageUnit>,
}

impl PreMergeDrainageUnits {
    /// Construct a terminal-first collection after records are materialized.
    pub(crate) fn new(
        terminal: UnitId,
        selected_level: SelectedLevel,
        units: Vec<PreMergeDrainageUnit>,
    ) -> Self {
        Self {
            terminal,
            selected_level,
            units,
        }
    }

    /// Construct a terminal-first collection for focused integration tests.
    #[cfg(feature = "test-fixtures")]
    pub fn new_for_test(
        terminal: UnitId,
        selected_level: SelectedLevel,
        units: Vec<PreMergeDrainageUnit>,
    ) -> Self {
        Self::new(terminal, selected_level, units)
    }

    /// Return the terminal unit ID represented by the first record.
    pub fn terminal(&self) -> UnitId {
        self.terminal
    }

    /// Return the whole terminal drainage-unit record.
    pub fn terminal_unit(&self) -> Option<&PreMergeDrainageUnit> {
        self.units.first()
    }

    /// Return the selected level shared by every record.
    pub fn selected_level(&self) -> SelectedLevel {
        self.selected_level
    }

    /// Return the terminal-first drainage-unit records.
    pub fn units(&self) -> &[PreMergeDrainageUnit] {
        &self.units
    }
}

/// Terminal-refinement result for the staged contract.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalRefinement {
    /// Refinement was disabled by the caller.
    Disabled,
    /// Best-effort refinement was visibly skipped.
    BestEffortSkipped {
        /// Provenance explaining why refinement was skipped.
        provenance: BestEffortRefinementProvenance,
    },
    /// Refinement produced a terminal geometry override.
    Applied {
        /// Refined outlet coordinate at the selected raster seed cell center.
        refined_outlet: GeoCoord,
        /// Refined terminal geometry used instead of the whole terminal polygon.
        geometry: ContainedTerminalPolygon,
        /// Provenance explaining why refinement ran.
        provenance: AppliedRefinementProvenance,
    },
}

impl TerminalRefinement {
    /// Construct a visible best-effort skip for a classified D8-path failure.
    pub fn best_effort_skipped(why: BestEffortSkipReason) -> Self {
        Self::BestEffortSkipped {
            provenance: BestEffortRefinementProvenance::new(
                RefinementStrategyName::BestEffortD8IfPresent,
                why,
            ),
        }
    }

    /// Construct a visible best-effort skip for missing D8 declarations.
    pub fn best_effort_no_d8_aux_declared() -> Self {
        Self::BestEffortSkipped {
            provenance: BestEffortRefinementProvenance::new(
                RefinementStrategyName::BestEffortD8IfPresent,
                BestEffortSkipReason::NoD8AuxDeclared,
            ),
        }
    }

    /// Construct explicit coarse provenance for unit-only containment without D8.
    pub fn best_effort_coarse_unit_only_no_d8_aux_declared() -> Self {
        Self::BestEffortSkipped {
            provenance: BestEffortRefinementProvenance::new(
                RefinementStrategyName::BestEffortD8IfPresent,
                BestEffortSkipReason::CoarseUnitOnlyNoD8AuxDeclared,
            ),
        }
    }

    /// Construct a visible best-effort skip for a retained unreadable D8 declaration.
    pub fn best_effort_unreadable_d8_aux_declared(schema: String) -> Self {
        Self::best_effort_skipped(BestEffortSkipReason::UnreadableD8AuxDeclared { schema })
    }

    /// Construct a visible best-effort skip for a missing raster source.
    pub fn best_effort_no_raster_source_provided() -> Self {
        Self::BestEffortSkipped {
            provenance: BestEffortRefinementProvenance::new(
                RefinementStrategyName::BestEffortD8IfPresent,
                BestEffortSkipReason::NoRasterSourceProvided,
            ),
        }
    }
}

/// Final dissolved watershed geometry and computed geodesic area.
#[derive(Debug, Clone, PartialEq)]
pub struct DissolvedWatershed {
    geometry: MultiPolygon<f64>,
    area_km2: AreaKm2,
}

impl DissolvedWatershed {
    /// Construct a dissolved watershed from assembled geometry and area.
    pub(crate) fn new(geometry: MultiPolygon<f64>, area_km2: AreaKm2) -> Self {
        Self { geometry, area_km2 }
    }

    /// Return the dissolved watershed geometry.
    pub fn geometry(&self) -> &MultiPolygon<f64> {
        &self.geometry
    }

    /// Return the geodesic watershed area in km².
    pub fn area_km2(&self) -> AreaKm2 {
        self.area_km2
    }
}

// ── UpstreamReaches ───────────────────────────────────────────────────────────

/// One reach centreline inside a delineated watershed.
///
/// Materialized from the HFX snap auxiliary the engine already reads during
/// outlet resolution. The geometry is the reach the fabric declares, not a
/// raster-traced channel, so its length is a property of the source fabric's
/// linework rather than of any threshold chosen here.
#[derive(Debug, Clone, PartialEq)]
pub struct DrainageReach {
    snap_id: SnapId,
    unit_id: UnitId,
    stem_role: Option<StemRole>,
    weight: Weight,
    length: ChannelLengthKm,
    geometry: Geometry<f64>,
    geometry_wkb: Vec<u8>,
}

impl DrainageReach {
    /// Construct a reach record.
    pub(crate) fn new(
        snap_id: SnapId,
        unit_id: UnitId,
        stem_role: Option<StemRole>,
        weight: Weight,
        length: ChannelLengthKm,
        geometry: Geometry<f64>,
        geometry_wkb: Vec<u8>,
    ) -> Self {
        Self {
            snap_id,
            unit_id,
            stem_role,
            weight,
            length,
            geometry,
            geometry_wkb,
        }
    }

    /// Return the snap-feature ID of this reach.
    pub fn snap_id(&self) -> SnapId {
        self.snap_id
    }

    /// Return the drainage unit this reach belongs to.
    pub fn unit_id(&self) -> UnitId {
        self.unit_id
    }

    /// Return the declared stem role, when the fabric declares one.
    ///
    /// On a DAG fabric such as GRIT, `Distributary` marks an anabranch — a
    /// channel leaving the mainstem rather than joining it. A dendritic fabric
    /// never declares one.
    pub fn stem_role(&self) -> Option<StemRole> {
        self.stem_role
    }

    /// Return the fabric's hydrologic weight for this reach.
    pub fn weight(&self) -> Weight {
        self.weight
    }

    /// Return the geodesic length of this reach.
    pub fn length(&self) -> ChannelLengthKm {
        self.length
    }

    /// Return the decoded reach centreline.
    pub fn geometry(&self) -> &Geometry<f64> {
        &self.geometry
    }

    /// Return the reach centreline as the fabric stored it, in OGC WKB.
    ///
    /// These are the bytes read from the snap auxiliary, not a re-encoding of
    /// the decoded geometry, so a caller handing them to another library sees
    /// exactly what the dataset declares.
    pub fn geometry_wkb(&self) -> &[u8] {
        &self.geometry_wkb
    }
}

/// The channel network of a delineated watershed.
///
/// Produced by [`Engine::upstream_reaches`](crate::engine::Engine::upstream_reaches).
/// Membership is exactly the reaches whose `unit_id` is in the delineated set:
/// the query is a bbox sweep, so reaches from neighbouring basins that fall
/// inside the same rectangle are discarded rather than counted.
#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamReaches {
    terminal: UnitId,
    selected_level: SelectedLevel,
    reaches: Vec<DrainageReach>,
}

impl UpstreamReaches {
    /// Construct a channel-network collection.
    pub(crate) fn new(
        terminal: UnitId,
        selected_level: SelectedLevel,
        reaches: Vec<DrainageReach>,
    ) -> Self {
        Self {
            terminal,
            selected_level,
            reaches,
        }
    }

    /// Return the terminal unit of the watershed these reaches drain.
    pub fn terminal(&self) -> UnitId {
        self.terminal
    }

    /// Return the HFX level these reaches were read at.
    pub fn selected_level(&self) -> SelectedLevel {
        self.selected_level
    }

    /// Return the reach records.
    pub fn reaches(&self) -> &[DrainageReach] {
        &self.reaches
    }

    /// Return the number of reaches.
    pub fn len(&self) -> usize {
        self.reaches.len()
    }

    /// Return `true` when the watershed carries no declared reaches.
    pub fn is_empty(&self) -> bool {
        self.reaches.is_empty()
    }

    /// Return the summed geodesic length of every reach — the numerator of
    /// drainage density.
    ///
    /// Kept here rather than left to callers because the sum must be taken over
    /// the same geodesic measure each reach was measured with; a caller
    /// re-measuring in a projection would silently change the ratio.
    pub fn total_length(&self) -> ChannelLengthKm {
        self.reaches.iter().map(DrainageReach::length).sum()
    }

    /// Return the summed geodesic length of reaches carrying `role`.
    pub fn total_length_of(&self, role: StemRole) -> ChannelLengthKm {
        self.reaches
            .iter()
            .filter(|reach| reach.stem_role() == Some(role))
            .map(DrainageReach::length)
            .sum()
    }

    /// Return the number of reaches carrying `role`.
    pub fn count_of(&self, role: StemRole) -> usize {
        self.reaches
            .iter()
            .filter(|reach| reach.stem_role() == Some(role))
            .count()
    }
}
