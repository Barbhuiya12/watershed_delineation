//! Compute the full morphometric profile of one basin from a live HFX dataset.
//!
//! Exercises the staged engine path end to end and prints every characteristic
//! reachable without an external raster: areal, planform, network topology, and
//! network geometry. Refinement is left disabled so the run needs no GDAL
//! raster source — the areas reported are whole-unit areas.
//!
//! ```text
//! cargo run --release --example basin_metrics -- \
//!     --dataset https://basin-delineations-public.upstream.tech/grit/hfx-v0.3.0/ \
//!     --lat 35.61 --lon -82.58
//! ```

use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::time::Instant;

use hfx::{StemRole, UnitId};
use pourpoint_core::algo::GeoCoord;
use pourpoint_core::resolver::{ResolverConfig, SearchRadiusMetres, SnapStrategy};
use pourpoint_core::session::DatasetSession;
use pourpoint_core::{DelineationOptions, Engine, LevelSelection, RefinementMode};

/// French Broad River at Asheville, NC — the outlet delineator's own
/// pourpoint demo uses, so the areas are directly comparable.
const DEFAULT_LAT: f64 = 35.61;
const DEFAULT_LON: f64 = -82.58;

struct Args {
    dataset: String,
    points: Vec<(String, f64, f64)>,
    snap_radius_m: Option<f64>,
    distance_first: bool,
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args {
        dataset: String::new(),
        points: Vec::new(),
        snap_radius_m: None,
        distance_first: false,
    };
    let mut it = env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--dataset" => args.dataset = it.next().ok_or("--dataset requires a value")?,
            // --point "Name" LAT LON, repeatable; every basin shares one session.
            "--point" => {
                let name = it.next().ok_or("--point requires NAME LAT LON")?;
                let lat = it.next().ok_or("--point requires a latitude")?.parse()?;
                let lon = it.next().ok_or("--point requires a longitude")?.parse()?;
                args.points.push((name, lat, lon));
            }
            "--distance-first" => args.distance_first = true,
            "--snap-radius" => {
                args.snap_radius_m =
                    Some(it.next().ok_or("--snap-radius requires metres")?.parse()?);
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    if args.dataset.is_empty() {
        return Err("--dataset is required".into());
    }
    if args.points.is_empty() {
        args.points
            .push(("French Broad @ Asheville".into(), DEFAULT_LAT, DEFAULT_LON));
    }
    Ok(args)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;

    let t0 = Instant::now();
    let session = DatasetSession::open(&args.dataset)?;
    println!("dataset open in {:.1} s", t0.elapsed().as_secs_f64());

    let engine = Engine::builder(session).build();
    let mut options = DelineationOptions::default().with_refinement_mode(RefinementMode::Disabled);
    if args.snap_radius_m.is_some() || args.distance_first {
        let mut config = ResolverConfig::new();
        if let Some(metres) = args.snap_radius_m {
            config = config.with_search_radius(SearchRadiusMetres::new(metres)?);
        }
        if args.distance_first {
            config = config.with_snap_strategy(SnapStrategy::DistanceFirst);
        }
        println!(
            "resolver: radius {} m, strategy {:?}",
            args.snap_radius_m.unwrap_or(1000.0),
            if args.distance_first {
                "DistanceFirst"
            } else {
                "WeightFirst"
            }
        );
        options = options.with_resolver_config(config);
    }

    for (name, lat, lon) in &args.points {
        println!("\n\n═══ {name}  ({lat}, {lon}) ═══");
        if let Err(e) = profile_basin(&engine, &options, GeoCoord::new(*lon, *lat)) {
            // One isolation point: a basin that fails must not abort the rest.
            println!("  FAILED: {e}");
        }
    }
    Ok(())
}

fn profile_basin(
    engine: &Engine,
    options: &DelineationOptions,
    outlet: GeoCoord,
) -> Result<(), Box<dyn Error>> {
    // ── Staged path, so the intermediates are available for metrics ─────────
    let t0 = Instant::now();
    let level = engine.select_level(LevelSelection::Finest)?;
    let resolved = engine.resolve_outlet_at_level(outlet, level, options.resolver_config())?;
    let upstream = engine.traverse_upstream_at_level(&resolved)?;
    let units = engine.produce_pre_merge_units(&upstream)?;
    let refinement = engine.refine_terminal(&resolved, &units, &options)?;
    let dissolved = engine.dissolve_watershed(&units, &refinement, &options)?;
    let result = engine.compose_result(resolved, upstream, &units, refinement, dissolved);
    let delineate_secs = t0.elapsed().as_secs_f64();

    // ── Channel network — the new stage ─────────────────────────────────────
    let t0 = Instant::now();
    let reaches = engine.upstream_reaches(&units)?;
    let reach_secs = t0.elapsed().as_secs_f64();

    // ── Areal and planform ──────────────────────────────────────────────────
    let area = result.area_km2().as_f64();
    let perimeter = result.perimeter_km()?.as_f64();
    let basin_length = basin_length_km(&result);

    println!("\n── areal + planform ───────────────────────────────");
    println!(
        "terminal unit           {}",
        result.terminal_unit_id().get()
    );
    println!("resolution              {:?}", result.resolution_method());
    println!("area                    {area:.2} km²");
    println!("perimeter               {perimeter:.2} km");
    println!("basin length  Lb        {basin_length:.2} km");
    println!(
        "Gravelius     Kc        {:.4}",
        perimeter / (2.0 * (std::f64::consts::PI * area).sqrt())
    );
    println!(
        "circularity   Rc        {:.4}",
        4.0 * std::f64::consts::PI * area / (perimeter * perimeter)
    );
    println!(
        "form factor   Rf        {:.4}",
        area / (basin_length * basin_length)
    );
    println!(
        "elongation    Re        {:.4}",
        (2.0 / basin_length) * (area / std::f64::consts::PI).sqrt()
    );

    // ── Network topology, from patch #3's traversal edges ───────────────────
    let orders = strahler_orders(&result);
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for order in orders.values() {
        *counts.entry(*order).or_default() += 1;
    }
    let max_order = counts.keys().copied().max().unwrap_or(0);

    println!("\n── network topology ───────────────────────────────");
    println!(
        "drainage units          {}",
        result.upstream_unit_ids().len()
    );
    println!("max Strahler order      {max_order}");
    for order in 1..=max_order {
        let n = counts.get(&order).copied().unwrap_or(0);
        let next = counts.get(&(order + 1)).copied().unwrap_or(0);
        if next > 0 {
            println!(
                "  order {order}: {n:>6} units   Rb({order}/{}) = {:.2}",
                order + 1,
                n as f64 / next as f64
            );
        } else {
            println!("  order {order}: {n:>6} units");
        }
    }

    // ── Network geometry, from patch #2 ────────────────────────────────────
    let total_length = reaches.total_length().as_f64();
    let mainstem = reaches.total_length_of(StemRole::Mainstem).as_f64();
    let dd = total_length / area;

    println!("\n── network geometry ───────────────────────────────");
    println!("reaches                 {}", reaches.len());
    println!("channel length ΣL       {total_length:.2} km");
    println!("mainstem length         {mainstem:.2} km");
    println!("drainage density Dd     {dd:.4} km/km²");
    println!(
        "stream frequency Fs     {:.4} /km²",
        reaches.len() as f64 / area
    );
    println!("channel maintenance C   {:.4} km²/km", 1.0 / dd);
    println!("overland flow    Lg     {:.4} km", 1.0 / (2.0 * dd));
    println!(
        "texture ratio    T      {:.4} /km",
        reaches.len() as f64 / perimeter
    );
    println!(
        "  mainstem / tributary / distributary   {} / {} / {}",
        reaches.count_of(StemRole::Mainstem),
        reaches.count_of(StemRole::Tributary),
        reaches.count_of(StemRole::Distributary),
    );

    println!("\ndelineate {delineate_secs:.1} s · reaches {reach_secs:.1} s");
    Ok(())
}

/// Longest geodesic distance from the resolved outlet to the basin boundary.
fn basin_length_km(result: &pourpoint_core::DelineationResult) -> f64 {
    use pourpoint_core::algo::geodesic_distance;
    let outlet = result.resolved_outlet();
    result
        .geometry()
        .0
        .iter()
        .flat_map(|polygon| polygon.exterior().coords())
        .map(|c| geodesic_distance(outlet, GeoCoord::new(c.x, c.y)).as_km())
        .fold(0.0_f64, f64::max)
}

/// Strahler order per unit, folded up the traversal tree from patch #3.
fn strahler_orders(result: &pourpoint_core::DelineationResult) -> HashMap<UnitId, u32> {
    // children[downstream] = [upstream, ...]
    let mut children: HashMap<UnitId, Vec<UnitId>> = HashMap::new();
    let mut terminal = result.terminal_unit_id();
    for unit in result.upstream_units() {
        match unit.downstream_id() {
            Some(down) => children.entry(down).or_default().push(unit.id()),
            None => terminal = unit.id(),
        }
    }

    // Post-order walk without recursion: a unit is ready once every child has
    // an order, which is what the second visit in this stack guarantees.
    let mut order: HashMap<UnitId, u32> = HashMap::new();
    let mut stack = vec![(terminal, false)];
    while let Some((id, expanded)) = stack.pop() {
        let kids = children.get(&id).map(Vec::as_slice).unwrap_or(&[]);
        if !expanded {
            stack.push((id, true));
            for &kid in kids {
                stack.push((kid, false));
            }
            continue;
        }
        if kids.is_empty() {
            order.insert(id, 1);
            continue;
        }
        let mut max = 0;
        let mut max_count = 0;
        for kid in kids {
            let o = order.get(kid).copied().unwrap_or(1);
            if o > max {
                max = o;
                max_count = 1;
            } else if o == max {
                max_count += 1;
            }
        }
        order.insert(id, if max_count > 1 { max + 1 } else { max });
    }
    order
}
