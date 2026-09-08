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

const INDEX_HTML: &str = include_str!("portal.html");
const DEFAULT_PORT: u16 = 8787;

/// Search radii tried in order when the caller does not pin one.
///
/// A map click lands wherever the cursor was, which is routinely a few hundred
/// metres off any channel — and on a wide river, several kilometres. Failing
/// the first attempt is correct engine behaviour; refusing to widen is bad app
/// behaviour. The response reports which radius won and how far it reached, so
/// a suspiciously distant snap stays visible rather than silently accepted.
const RADIUS_LADDER: [f64; 4] = [1_000.0, 5_000.0, 20_000.0, 50_000.0];

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

    let reach_features: Vec<Value> = reaches
        .reaches()
        .iter()
        .map(|reach| {
            let geom = if tol > 0.0 {
                simplify_geometry(reach.geometry(), tol)
            } else {
                reach.geometry().clone()
            };
            json!({
                "type": "Feature",
                "geometry": line_json(&geom),
                "properties": {
                    "unit_id": reach.unit_id().get(),
                    "snap_id": reach.snap_id().get(),
                    "stem_role": reach.stem_role().map(|r| r.to_string()),
                    "length_km": reach.length().as_f64(),
                }
            })
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

/// Douglas-Peucker tolerance in degrees for the geometry sent to the browser.
///
/// Every reported metric is computed on the FULL geometry before this runs —
/// simplification is display-only. A continental basin returns tens of
/// thousands of reaches, which a map cannot draw at full fidelity anyway, so
/// the tolerance scales with extent: tight enough to be invisible at the zoom
/// the basin is viewed at, loose enough to keep the payload sane.
fn display_tolerance(area_km2: f64) -> f64 {
    match area_km2 {
        a if a < 1_000.0 => 0.0,     // small basins ship untouched
        a if a < 10_000.0 => 0.0005, // ~55 m
        a if a < 100_000.0 => 0.002, // ~220 m
        _ => 0.005,                  // ~550 m
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

/// Simplify a reach centreline for display; non-linear input passes through.
fn simplify_geometry(g: &geo::Geometry<f64>, tol: f64) -> geo::Geometry<f64> {
    use geo::Simplify;
    match g {
        geo::Geometry::LineString(ls) => geo::Geometry::LineString(ls.simplify(&tol)),
        geo::Geometry::MultiLineString(mls) => geo::Geometry::MultiLineString(mls.simplify(&tol)),
        other => other.clone(),
    }
}

/// GeoJSON geometry for a reach centreline; non-linear input yields `null`.
fn line_json(g: &geo::Geometry<f64>) -> Value {
    match g {
        geo::Geometry::LineString(ls) => {
            json!({ "type": "LineString", "coordinates": ring_json(ls) })
        }
        geo::Geometry::MultiLineString(mls) => json!({
            "type": "MultiLineString",
            "coordinates": mls.0.iter().map(ring_json).collect::<Vec<_>>()
        }),
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
