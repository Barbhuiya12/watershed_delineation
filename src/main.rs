//! MG Hydro Watershed Delineation System — a local portal over `pourpoint-core`.
//!
//! Opens the HFX dataset once at startup, then serves live delineations over
//! HTTP. Uses only the standard library for transport, so it adds no
//! dependencies to the workspace, and no GDAL: terminal refinement is disabled,
//! so every area is a whole-unit union.
//!
//! ```text
//! cargo run --release --example portal_server -- \
//!     --dataset https://basin-delineations-public.upstream.tech/grit/hfx-v0.3.0/ --port 8787
//! ```

use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::time::Instant;

use hfx::{StemRole, UnitId};
use pourpoint_core::algo::coord::GeoCoord;
use pourpoint_core::algo::geodesic_distance;
use pourpoint_core::resolver::{ResolverConfig, SearchRadiusMetres, SnapStrategy};
use pourpoint_core::session::DatasetSession;
use pourpoint_core::{DelineationOptions, Engine, LevelSelection, RefinementMode};
use serde_json::{Value, json};
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::core::solver::Solver;
use i_overlay::float::overlay::FloatOverlay;
use i_overlay::float::string_overlay::FloatStringOverlay;
use i_overlay::string::clip::ClipRule;

const INDEX_HTML: &str = include_str!("portal.html");
const DEFAULT_PORT: u16 = 8787;

/// Search radii tried in order when the caller does not pin one.
const RADIUS_LADDER: [f64; 5] = [100.0, 300.0, 1_000.0, 5_000.0, 20_000.0];

fn main() -> Result<(), Box<dyn Error>> {
    let mut dataset = env::var("INCLINE_DATASET_PATH")
        .or_else(|_| env::var("HFX_DATASET_PATH"))
        .unwrap_or_default();
    let mut port = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let mut host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

    let mut it = env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--dataset" => dataset = it.next().ok_or("--dataset requires a value")?,
            "--port" => port = it.next().ok_or("--port requires a value")?.parse()?,
            "--host" => host = it.next().ok_or("--host requires a value")?,
            "--help" | "-h" => {
                println!("INCLINE Watershed Delineation Server");
                println!("Usage: incline-watershed-system --dataset <PATH_OR_URL> [--port <PORT>] [--host <HOST>]");
                println!("\nEnvironment variables:");
                println!("  INCLINE_DATASET_PATH or HFX_DATASET_PATH : dataset path or cloud URL");
                println!("  PORT                                     : HTTP listen port (default: 8787)");
                println!("  HOST                                     : HTTP listen host (default: 0.0.0.0)");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    if dataset.is_empty() {
        return Err("--dataset is required (or set INCLINE_DATASET_PATH)".into());
    }

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║       INCLINE — High-Performance Watershed Delineation       ║");
    println!("║             IIT Mandi · Hydrographic Engine                  ║");
    println!("║       Team: Siddik Barbhuiya & Dr. Vivek Gupta               ║");
    println!("║       Powered by pourpoint-core (by Nicolas Lazaro)          ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!("opening dataset {dataset} …");
    let t0 = Instant::now();
    let session = DatasetSession::open(&dataset)?;
    let open_secs = t0.elapsed().as_secs_f64();
    println!("dataset ready in {open_secs:.2} s");

    let engine = Arc::new(Mutex::new(Engine::builder(session).build()));
    let bind_addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&bind_addr)?;
    println!("\n  ▶  INCLINE Web Portal : http://127.0.0.1:{port}");
    println!("  ▶  Bound on address   : {bind_addr}\n");
    println!("Server running — press Ctrl-C to stop");

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let engine = Arc::clone(&engine);
        let dataset = dataset.clone();
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
            if let Err(e) = handle(stream, &engine, &dataset, open_secs) {
                eprintln!("request failed: {e}");
            }
        });
    }
    Ok(())
}

fn handle(
    mut stream: TcpStream,
    engine: &Mutex<Engine>,
    dataset: &str,
    open_secs: f64,
) -> Result<(), Box<dyn Error>> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/").to_string();

    if method == "OPTIONS" {
        return send_cors_preflight(&mut stream);
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, parse_query(q)),
        None => (target.as_str(), HashMap::new()),
    };

    match path {
        "/" | "/index.html" => send(&mut stream, 200, "text/html; charset=utf-8", INDEX_HTML),
        "/api/meta" => {
            let body = json!({
                "system": "INCLINE Watershed Delineation System",
                "institution": "IIT Mandi",
                "team": [
                    "Siddik Barbhuiya",
                    "Dr. Vivek Gupta"
                ],
                "engine": "pourpoint-core (by Nicolas Lazaro)",
                "dataset": dataset,
                "open_seconds": open_secs
            });
            send(&mut stream, 200, "application/json", &body.to_string())
        }
        "/api/delineate" => {
            let guard = match engine.lock() {
                Ok(g) => g,
                Err(poisoned) => poisoned.into_inner(),
            };
            let (code, body) = delineate(&guard, &query);
            send(&mut stream, code, "application/json", &body.to_string())
        }
        _ => send(&mut stream, 404, "text/plain", "not found"),
    }
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.replace("%2D", "-").replace('+', " ")))
        .collect()
}

fn send_cors_preflight(stream: &mut TcpStream) -> Result<(), Box<dyn Error>> {
    write!(
        stream,
        "HTTP/1.1 204 No Content\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
        Access-Control-Allow-Headers: *\r\n\
        Access-Control-Max-Age: 86400\r\n\
        Connection: close\r\n\r\n"
    )?;
    stream.flush()?;
    Ok(())
}

fn send(stream: &mut TcpStream, code: u16, ctype: &str, body: &str) -> Result<(), Box<dyn Error>> {
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\n\
        Content-Type: {ctype}\r\n\
        Content-Length: {}\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
        Access-Control-Allow-Headers: *\r\n\
        Cache-Control: no-store\r\n\
        Connection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()?;
    Ok(())
}

fn delineate(engine: &Engine, q: &HashMap<String, String>) -> (u16, Value) {
    let parse = |k: &str| q.get(k).and_then(|v| v.parse::<f64>().ok());
    let (Some(lat), Some(lon)) = (parse("lat"), parse("lon")) else {
        return (400, json!({ "error": "lat and lon are required" }));
    };

    let distance_first = q.get("strategy").map(String::as_str) == Some("distance");
    let base = |metres: f64| -> Result<ResolverConfig, &'static str> {
        let mut c = ResolverConfig::new().with_search_radius(SearchRadiusMetres::new(metres)?);
        if distance_first {
            c = c.with_snap_strategy(SnapStrategy::DistanceFirst);
        }
        Ok(c)
    };

    // A pinned radius is honoured exactly; otherwise walk the ladder.
    let ladder: Vec<f64> = match parse("radius") {
        Some(m) => vec![m],
        None => RADIUS_LADDER.to_vec(),
    };

    let outlet = GeoCoord::new(lon, lat);
    let t0 = Instant::now();

    let level = match engine.select_level(LevelSelection::Finest) {
        Ok(l) => l,
        Err(e) => return (200, json!({ "ok": false, "error": e.to_string() })),
    };

    let mut resolved = None;
    let mut radius_used = 0.0;
    let mut last_err = String::new();
    for (i, metres) in ladder.iter().enumerate() {
        let config = match base(*metres) {
            Ok(c) => c,
            Err(e) => return (400, json!({ "error": e })),
        };
        match engine.resolve_outlet_at_level(outlet, level, &config) {
            Ok(v) => {
                resolved = Some((v, config));
                radius_used = *metres;
                break;
            }
            Err(e) => {
                last_err = e.to_string();
                if i + 1 == ladder.len() {
                    return (
                        200,
                        json!({ "ok": false, "error": last_err, "radius_exhausted": *metres }),
                    );
                }
            }
        }
    }
    let Some((resolved, config)) = resolved else {
        return (200, json!({ "ok": false, "error": last_err }));
    };
    let escalated = radius_used > ladder[0] || (ladder.len() > 1 && radius_used > RADIUS_LADDER[0]);

    let options = DelineationOptions::default()
        .with_refinement_mode(RefinementMode::Disabled)
        .with_resolver_config(config);

    let mut t_traverse = 0.0;
    let mut t_units = 0.0;
    let mut t_dissolve = 0.0;
    let mut finest_units = 0usize;
    let staged = (|| {
        let a = Instant::now();
        let upstream = engine.traverse_upstream_at_level(&resolved)?;
        finest_units = upstream.upstream().len();
        t_traverse = a.elapsed().as_secs_f64();

        let a = Instant::now();
        let units = engine.produce_pre_merge_units(&upstream)?;
        t_units = a.elapsed().as_secs_f64();

        let refinement = engine.refine_terminal(&resolved, &units, &options)?;

        let a = Instant::now();
        let dissolved = engine.dissolve_watershed(&units, &refinement, &options)?;
        t_dissolve = a.elapsed().as_secs_f64();

        Ok::<_, pourpoint_core::EngineError>((
            engine.compose_result(resolved, upstream, &units, refinement, dissolved),
            units,
        ))
    })();

    let (result, units) = match staged {
        Ok(v) => v,
        Err(e) => return (200, json!({ "ok": false, "error": e.to_string() })),
    };
    let delineate_secs = t0.elapsed().as_secs_f64();

    let t1 = Instant::now();
    let reaches = match engine.upstream_reaches(&units) {
        Ok(r) => r,
        Err(e) => return (200, json!({ "ok": false, "error": e.to_string() })),
    };
    let reach_secs = t1.elapsed().as_secs_f64();

    let area = result.area_km2().as_f64();
    let perimeter = match result.perimeter_km() {
        Ok(p) => p.as_f64(),
        Err(e) => return (200, json!({ "ok": false, "error": e.to_string() })),
    };
    let lb = basin_length_km(&result);
    let total = reaches.total_length().as_f64();
    let n = reaches.len() as f64;
    let pi = std::f64::consts::PI;

    let orders = strahler_orders(&result);
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for o in orders.values() {
        *counts.entry(*o).or_default() += 1;
    }
    let max_order = counts.keys().copied().max().unwrap_or(0);
    let order_counts: Vec<usize> = (1..=max_order)
        .map(|o| counts.get(&o).copied().unwrap_or(0))
        .collect();

    let tol = display_tolerance(area);
    let display_watershed = if tol > 0.0 {
        use geo::Simplify;
        result.geometry().simplify(&tol)
    } else {
        result.geometry().clone()
    };

    let terminal_id = units.terminal();
    let res_coord = [result.resolved_outlet().lon, result.resolved_outlet().lat];

    // Find the terminal reach: first try by unit_id == terminal_id, fallback to reach closest to res_coord
    let term_reach_idx = reaches
        .reaches()
        .iter()
        .position(|r| r.unit_id() == terminal_id)
        .or_else(|| {
            let mut best_idx = None;
            let mut best_d = f64::INFINITY;
            for (idx, r) in reaches.reaches().iter().enumerate() {
                match r.geometry() {
                    geo::Geometry::LineString(ls) => {
                        for pt in &ls.0 {
                            let d = dist_sq(res_coord, [pt.x, pt.y]);
                            if d < best_d {
                                best_d = d;
                                best_idx = Some(idx);
                            }
                        }
                    }
                    geo::Geometry::MultiLineString(mls) => {
                        for ls in &mls.0 {
                            for pt in &ls.0 {
                                let d = dist_sq(res_coord, [pt.x, pt.y]);
                                if d < best_d {
                                    best_d = d;
                                    best_idx = Some(idx);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            best_idx
        });

    let other_geom_refs: Vec<&geo::Geometry<f64>> = reaches
        .reaches()
        .iter()
        .enumerate()
        .filter(|(idx, _)| Some(*idx) != term_reach_idx)
        .map(|(_, r)| r.geometry())
        .collect();

    let term_reach_geom = term_reach_idx.map(|idx| {
        truncate_terminal_reach(reaches.reaches()[idx].geometry(), res_coord, &other_geom_refs)
    });

    // Preserve the complete true topographical watershed divide. Slicing with a global half-plane
    // cuts off meandering basins and tributary arms that wrap around mountain ranges (e.g. Sutlej).
    // The terminal reach itself is cleanly truncated at res_coord by truncate_terminal_reach.
    let display_watershed = display_watershed;

    let shapes = multipolygon_to_shapes(&display_watershed);

    let reach_features: Vec<Value> = reaches
        .reaches()
        .iter()
        .enumerate()
        .filter_map(|(idx, reach)| {
            let base_geom = if Some(idx) == term_reach_idx {
                let term = term_reach_geom.clone().unwrap_or_else(|| reach.geometry().clone());
                clip_reach_geometry(&term, &shapes)
            } else {
                reach.geometry().clone()
            };
            let geom_json = line_json(&base_geom);
            if geom_json.is_null() {
                return None;
            }
            Some(json!({
                "type": "Feature",
                "geometry": geom_json,
                "properties": {
                    "unit_id": reach.unit_id().get(),
                    "snap_id": reach.snap_id().get(),
                    "stem_role": reach.stem_role().map(|r| r.to_string()),
                    "length_km": reach.length().as_f64(),
                }
            }))
        })
        .collect();

    (
        200,
        json!({
            "ok": true,
            "input": { "lat": lat, "lon": lon },
            "radius_used_m": radius_used,
            "escalated": escalated,
            "finest_units": finest_units,
            "terminal_unit_id": result.terminal_unit_id().get(),
            "resolved": { "lat": result.resolved_outlet().lat, "lon": result.resolved_outlet().lon },
            "resolution": format!("{:?}", result.resolution_method()),
            "area_km2": area,
            "perimeter_km": perimeter,
            "basin_length_km": lb,
            "gravelius_kc": perimeter / (2.0 * (pi * area).sqrt()),
            "circularity_rc": 4.0 * pi * area / (perimeter * perimeter),
            "form_factor_rf": area / (lb * lb),
            "elongation_re": (2.0 / lb) * (area / pi).sqrt(),
            "units": result.upstream_unit_ids().len(),
            "max_strahler_order": max_order,
            "order_counts": order_counts,
            "reaches": reaches.len(),
            "channel_length_km": total,
            "mainstem_length_km": reaches.total_length_of(StemRole::Mainstem).as_f64(),
            "drainage_density": total / area,
            "stream_frequency": n / area,
            "channel_maintenance": area / total,
            "overland_flow_km": area / (2.0 * total),
            "texture_ratio": (order_counts.first().copied().unwrap_or(1) as f64) / perimeter,
            "texture_ratio_total": n / perimeter,
            "roles": {
                "mainstem": reaches.count_of(StemRole::Mainstem),
                "tributary": reaches.count_of(StemRole::Tributary),
                "distributary": reaches.count_of(StemRole::Distributary),
            },
            "timing": {
                "delineate_s": delineate_secs,
                "reaches_s": reach_secs,
                "traverse_s": t_traverse,
                "fetch_units_s": t_units,
                "dissolve_s": t_dissolve,
                "serialize_s": 0.0
            },
            "geometry": {
                "simplified_deg": tol,
                "watershed": multipolygon_json(&display_watershed),
                "reaches": {
                    "type": "FeatureCollection",
                    "features": reach_features
                }
            },
        }),
    )
}

/// Tolerance in degrees for boundary simplification. Set to 0.0 (full fidelity)
/// so that the boundary matches the true watershed divide and reaches never protrude.
fn display_tolerance(_area_km2: f64) -> f64 {
    0.0
}

/// Project point `p` onto line segment `[a, b]`, returning `(projected_point, t)`.
fn project_point_to_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> ([f64; 2], f64) {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let len_sq = dx * dx + dy * dy;
    if len_sq == 0.0 {
        return (a, 0.0);
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len_sq).clamp(0.0, 1.0);
    ([a[0] + t * dx, a[1] + t * dy], t)
}

fn dist_sq(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

/// Truncate a terminal reach so that it terminates precisely at the resolved outlet point,
/// discarding downstream overshoot and guaranteeing the stream tip connects to the outlet.
fn truncate_terminal_reach(
    geom: &geo::Geometry<f64>,
    res_pt: [f64; 2],
    other_reaches: &[&geo::Geometry<f64>],
) -> geo::Geometry<f64> {
    match geom {
        geo::Geometry::LineString(ls) => {
            if ls.0.len() < 2 {
                return geom.clone();
            }
            let coords: Vec<[f64; 2]> = ls.0.iter().map(|c| [c.x, c.y]).collect();

            // Find segment closest to res_pt
            let mut best_seg = 0;
            let mut best_d = f64::INFINITY;
            for i in 0..(coords.len() - 1) {
                let (proj, _) = project_point_to_segment(res_pt, coords[i], coords[i + 1]);
                let d = dist_sq(res_pt, proj);
                if d < best_d {
                    best_d = d;
                    best_seg = i;
                }
            }

            // Determine which endpoint connects to upstream reaches
            let mut min_d_start = f64::INFINITY;
            let mut min_d_end = f64::INFINITY;
            for o in other_reaches {
                match o {
                    geo::Geometry::LineString(ols) => {
                        for pt in [ols.0.first(), ols.0.last()].into_iter().flatten() {
                            let p = [pt.x, pt.y];
                            min_d_start = min_d_start.min(dist_sq(coords[0], p));
                            min_d_end = min_d_end.min(dist_sq(coords[coords.len() - 1], p));
                        }
                    }
                    geo::Geometry::MultiLineString(omls) => {
                        for ols in &omls.0 {
                            for pt in [ols.0.first(), ols.0.last()].into_iter().flatten() {
                                let p = [pt.x, pt.y];
                                min_d_start = min_d_start.min(dist_sq(coords[0], p));
                                min_d_end = min_d_end.min(dist_sq(coords[coords.len() - 1], p));
                            }
                        }
                    }
                    _ => {}
                }
            }

            let start_is_upstream = if other_reaches.is_empty() {
                dist_sq(coords[0], res_pt) >= dist_sq(coords[coords.len() - 1], res_pt) || best_seg > 0
            } else {
                min_d_start <= min_d_end
            };

            let mut new_coords: Vec<geo::Coord<f64>> = if start_is_upstream {
                coords[0..=best_seg]
                    .iter()
                    .map(|p| geo::Coord { x: p[0], y: p[1] })
                    .collect()
            } else {
                let mut rev: Vec<geo::Coord<f64>> = coords[(best_seg + 1)..]
                    .iter()
                    .map(|p| geo::Coord { x: p[0], y: p[1] })
                    .collect();
                rev.reverse();
                rev
            };

            // Explicitly pin the final coordinate to the resolved outlet marker
            new_coords.push(geo::Coord { x: res_pt[0], y: res_pt[1] });
            geo::Geometry::LineString(geo::LineString::new(new_coords))
        }
        geo::Geometry::MultiLineString(mls) => {
            let mut new_lines = Vec::new();
            for ls in &mls.0 {
                let geom_ls = geo::Geometry::LineString(ls.clone());
                new_lines.push(truncate_terminal_reach(&geom_ls, res_pt, other_reaches));
            }
            let lines: Vec<geo::LineString<f64>> = new_lines
                .into_iter()
                .filter_map(|g| match g {
                    geo::Geometry::LineString(l) if !l.0.is_empty() => Some(l),
                    _ => None,
                })
                .collect();
            if lines.len() == 1 {
                geo::Geometry::LineString(lines.into_iter().next().unwrap())
            } else {
                geo::Geometry::MultiLineString(geo::MultiLineString::new(lines))
            }
        }
        other => other.clone(),
    }
}

/// Slice the watershed MultiPolygon at the resolved outlet point perpendicular to the
/// stream direction, ensuring the watershed divide closes PRECISELY at the outlet marker
/// with 0 distance and 0 downstream overshoot.
/// Find the downstream flow direction tangent at the outlet by finding the stream
/// segment closest to res_pt. Works for any LineString or MultiLineString geometry.
#[allow(dead_code)]
fn get_stream_direction_at_outlet(
    geom: &geo::Geometry<f64>,
    res_pt: [f64; 2],
) -> Option<(f64, f64)> {
    let mut best_dir = None;
    let mut best_dist_sq = f64::INFINITY;

    let mut check_linestring = |ls: &geo::LineString<f64>| {
        for i in 0..(ls.0.len().saturating_sub(1)) {
            let p1 = [ls.0[i].x, ls.0[i].y];
            let p2 = [ls.0[i + 1].x, ls.0[i + 1].y];
            let dx = p2[0] - p1[0];
            let dy = p2[1] - p1[1];
            let mag = dx.hypot(dy);
            if mag < 1e-9 {
                continue;
            }
            let (proj, _) = project_point_to_segment(res_pt, p1, p2);
            let d = dist_sq(res_pt, proj);
            if d < best_dist_sq {
                best_dist_sq = d;
                best_dir = Some((dx / mag, dy / mag));
            }
        }
    };

    match geom {
        geo::Geometry::LineString(ls) => check_linestring(ls),
        geo::Geometry::MultiLineString(mls) => {
            for ls in &mls.0 {
                check_linestring(ls);
            }
        }
        _ => {}
    }

    best_dir
}

#[allow(dead_code)]
fn slice_watershed_at_outlet(
    mp: &geo::MultiPolygon<f64>,
    res_pt: [f64; 2],
    terminal_reach_geom: &geo::Geometry<f64>,
) -> geo::MultiPolygon<f64> {
    let dir = get_stream_direction_at_outlet(terminal_reach_geom, res_pt);

    let Some((dx, dy)) = dir else {
        return mp.clone();
    };
    slice_watershed_with_dir(mp, res_pt, dx, dy)
}

#[allow(dead_code)]
fn slice_watershed_with_dir(
    mp: &geo::MultiPolygon<f64>,
    res_pt: [f64; 2],
    dx: f64,
    dy: f64,
) -> geo::MultiPolygon<f64> {
    let nx = -dy;
    let ny = dx;

    let span = 15.0; // degrees (~1500 km)
    let upstream_span = 30.0; // degrees (~3000 km)

    let p_left = [res_pt[0] - nx * span, res_pt[1] - ny * span];
    let p_right = [res_pt[0] + nx * span, res_pt[1] + ny * span];
    let p_back_right = [p_right[0] - dx * upstream_span, p_right[1] - dy * upstream_span];
    let p_back_left = [p_left[0] - dx * upstream_span, p_left[1] - dy * upstream_span];

    // i_overlay contours must NOT have duplicate closing points (4 distinct vertices)
    let cutter_poly: Vec<[f64; 2]> = vec![p_left, p_right, p_back_right, p_back_left];
    let cutter_shape: Vec<Vec<Vec<[f64; 2]>>> = vec![vec![cutter_poly]];

    let shapes = multipolygon_to_shapes(mp);
    let overlay = FloatOverlay::with_subj_and_clip(&shapes, &cutter_shape);
    let sliced_shapes = overlay.overlay(OverlayRule::Intersect, FillRule::NonZero);

    if sliced_shapes.is_empty() {
        return mp.clone();
    }

    shapes_to_multipolygon(&sliced_shapes)
}

#[allow(dead_code)]
fn shapes_to_multipolygon(shapes: &Vec<Vec<Vec<[f64; 2]>>>) -> geo::MultiPolygon<f64> {
    let mut polys = Vec::new();
    for shape in shapes {
        if shape.is_empty() || shape[0].len() < 3 {
            continue;
        }
        let mut ext_pts: Vec<geo::Coord<f64>> = shape[0]
            .iter()
            .map(|pt| geo::Coord { x: pt[0], y: pt[1] })
            .collect();
        if let (Some(first), Some(last)) = (ext_pts.first(), ext_pts.last()) {
            if first != last {
                ext_pts.push(*first);
            }
        }
        let exterior = geo::LineString::new(ext_pts);
        let interiors: Vec<geo::LineString<f64>> = shape[1..]
            .iter()
            .filter(|ring| ring.len() >= 3)
            .map(|ring| {
                let mut int_pts: Vec<geo::Coord<f64>> = ring
                    .iter()
                    .map(|pt| geo::Coord { x: pt[0], y: pt[1] })
                    .collect();
                if let (Some(first), Some(last)) = (int_pts.first(), int_pts.last()) {
                    if first != last {
                        int_pts.push(*first);
                    }
                }
                geo::LineString::new(int_pts)
            })
            .collect();
        polys.push(geo::Polygon::new(exterior, interiors));
    }
    geo::MultiPolygon::new(polys)
}

/// Convert a `geo::MultiPolygon<f64>` into `i_overlay` shapes `Vec<Vec<Vec<[f64; 2]>>>`.
/// Removes duplicate closing vertices so `i_overlay` solvers receive valid open contours.
fn multipolygon_to_shapes(mp: &geo::MultiPolygon<f64>) -> Vec<Vec<Vec<[f64; 2]>>> {
    mp.0.iter()
        .map(|poly| {
            let mut contours = Vec::with_capacity(1 + poly.interiors().len());
            // Exterior ring (strip closing duplicate point for i_overlay)
            let mut ext: Vec<[f64; 2]> = poly.exterior().coords().map(|c| [c.x, c.y]).collect();
            if ext.len() > 1 && ext[0] == ext[ext.len() - 1] {
                ext.pop();
            }
            contours.push(ext);
            // Interior rings (holes)
            for interior in poly.interiors() {
                let mut hole: Vec<[f64; 2]> = interior.coords().map(|c| [c.x, c.y]).collect();
                if hole.len() > 1 && hole[0] == hole[hole.len() - 1] {
                    hole.pop();
                }
                contours.push(hole);
            }
            contours
        })
        .collect()
}

/// Clip a `geo::Geometry<f64>` against the watershed MultiPolygon shapes.
/// Guaranteed to keep only lines and segments that lie strictly inside or on the boundary.
fn clip_reach_geometry(
    geom: &geo::Geometry<f64>,
    shapes: &Vec<Vec<Vec<[f64; 2]>>>,
) -> geo::Geometry<f64> {
    let paths: Vec<Vec<[f64; 2]>> = match geom {
        geo::Geometry::LineString(ls) => {
            if ls.0.len() < 2 {
                return geom.clone();
            }
            vec![ls.0.iter().map(|c| [c.x, c.y]).collect()]
        }
        geo::Geometry::MultiLineString(mls) => {
            mls.0
                .iter()
                .filter(|ls| ls.0.len() >= 2)
                .map(|ls| ls.0.iter().map(|c| [c.x, c.y]).collect())
                .collect()
        }
        other => return other.clone(),
    };

    if paths.is_empty() || shapes.is_empty() {
        return geom.clone();
    }

    let overlay = FloatStringOverlay::with_shape_and_string(shapes, &paths);
    let clipped: Vec<Vec<[f64; 2]>> = overlay.clip_string_lines_with_solver(
        FillRule::NonZero,
        ClipRule {
            invert: false,
            boundary_included: true,
        },
        Solver::default(),
    );

    if clipped.is_empty() {
        geo::Geometry::LineString(geo::LineString::new(vec![]))
    } else if clipped.len() == 1 {
        let coords: Vec<geo::Coord<f64>> = clipped[0]
            .iter()
            .map(|pt| geo::Coord { x: pt[0], y: pt[1] })
            .collect();
        geo::Geometry::LineString(geo::LineString::new(coords))
    } else {
        let lines: Vec<geo::LineString<f64>> = clipped
            .into_iter()
            .map(|pts| {
                let coords = pts.into_iter().map(|pt| geo::Coord { x: pt[0], y: pt[1] }).collect();
                geo::LineString::new(coords)
            })
            .collect();
        geo::Geometry::MultiLineString(geo::MultiLineString::new(lines))
    }
}

/// Round to six decimals — roughly 0.1 m, well inside the fabric's precision,
/// and it roughly halves the payload for a basin with many vertices.
fn r6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

fn ring_json(ring: &geo::LineString<f64>) -> Value {
    Value::Array(
        ring.0
            .iter()
            .map(|c| json!([r6(c.x), r6(c.y)]))
            .collect::<Vec<_>>(),
    )
}

/// GeoJSON `MultiPolygon` geometry for the dissolved watershed.
fn multipolygon_json(mp: &geo::MultiPolygon<f64>) -> Value {
    let polys: Vec<Value> =
        mp.0.iter()
            .map(|poly| {
                let mut rings = vec![ring_json(poly.exterior())];
                rings.extend(poly.interiors().iter().map(ring_json));
                Value::Array(rings)
            })
            .collect();
    json!({ "type": "MultiPolygon", "coordinates": polys })
}

/// GeoJSON geometry for a reach centreline; non-linear or empty input yields `null`.
fn line_json(g: &geo::Geometry<f64>) -> Value {
    match g {
        geo::Geometry::LineString(ls) => {
            if ls.0.is_empty() {
                Value::Null
            } else {
                json!({ "type": "LineString", "coordinates": ring_json(ls) })
            }
        }
        geo::Geometry::MultiLineString(mls) => {
            let valid_lines: Vec<_> = mls.0.iter().filter(|ls| !ls.0.is_empty()).collect();
            if valid_lines.is_empty() {
                Value::Null
            } else if valid_lines.len() == 1 {
                json!({ "type": "LineString", "coordinates": ring_json(valid_lines[0]) })
            } else {
                json!({
                    "type": "MultiLineString",
                    "coordinates": valid_lines.iter().map(|ls| ring_json(ls)).collect::<Vec<_>>()
                })
            }
        }
        _ => Value::Null,
    }
}

/// Longest geodesic distance from the resolved outlet to the basin boundary.
fn basin_length_km(result: &pourpoint_core::DelineationResult) -> f64 {
    let outlet = result.resolved_outlet();
    result
        .geometry()
        .0
        .iter()
        .flat_map(|polygon| polygon.exterior().coords())
        .map(|c| geodesic_distance(outlet, GeoCoord::new(c.x, c.y)).as_km())
        .fold(0.0_f64, f64::max)
}

/// Strahler order per unit, folded up the traversal tree.
fn strahler_orders(result: &pourpoint_core::DelineationResult) -> HashMap<UnitId, u32> {
    let mut children: HashMap<UnitId, Vec<UnitId>> = HashMap::new();
    let mut terminal = result.terminal_unit_id();
    for unit in result.upstream_units() {
        match unit.downstream_id() {
            Some(down) => children.entry(down).or_default().push(unit.id()),
            None => terminal = unit.id(),
        }
    }

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
        let (mut max, mut max_count) = (0, 0);
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
