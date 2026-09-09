use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::solver::Solver;
use i_overlay::float::string_overlay::FloatStringOverlay;
use i_overlay::string::clip::ClipRule;

#[test]
fn test_line_clipping_outside_polygon() {
    // A square polygon [0, 10] x [0, 10]
    let poly: Vec<Vec<[f64; 2]>> = vec![vec![
        [0.0, 0.0],
        [10.0, 0.0],
        [10.0, 10.0],
        [0.0, 10.0],
    ]];
    let shapes: Vec<Vec<Vec<[f64; 2]>>> = vec![poly];

    // A line that starts inside and extends outside to 15.0
    let line: Vec<[f64; 2]> = vec![[5.0, 5.0], [15.0, 5.0]];
    let paths: Vec<Vec<[f64; 2]>> = vec![line];

    let overlay = FloatStringOverlay::with_shape_and_string(&shapes, &paths);
    let clipped: Vec<Vec<[f64; 2]>> = overlay.clip_string_lines_with_solver(
        FillRule::NonZero,
        ClipRule {
            invert: false,
            boundary_included: true,
        },
        Solver::default(),
    );

    println!("Clipped paths: {:?}", clipped);
    assert_eq!(clipped.len(), 1);
    // The line should be clipped at x = 10.0
    assert_eq!(clipped[0][0], [5.0, 5.0]);
    assert_eq!(clipped[0][1], [10.0, 5.0]);
}

#[test]
fn test_completely_outside_line_is_removed() {
    let poly: Vec<Vec<[f64; 2]>> = vec![vec![
        [0.0, 0.0],
        [10.0, 0.0],
        [10.0, 10.0],
        [0.0, 10.0],
    ]];
    let shapes: Vec<Vec<Vec<[f64; 2]>>> = vec![poly];

    // Completely outside
    let line: Vec<[f64; 2]> = vec![[15.0, 15.0], [20.0, 20.0]];
    let paths: Vec<Vec<[f64; 2]>> = vec![line];

    let overlay = FloatStringOverlay::with_shape_and_string(&shapes, &paths);
    let clipped: Vec<Vec<[f64; 2]>> = overlay.clip_string_lines_with_solver(
        FillRule::NonZero,
        ClipRule {
            invert: false,
            boundary_included: true,
        },
        Solver::default(),
    );

    assert!(clipped.is_empty());
}

#[test]
fn test_polygon_intersection() {
    use i_overlay::float::overlay::FloatOverlay;
    use i_overlay::core::overlay_rule::OverlayRule;

    let poly_a: Vec<Vec<[f64; 2]>> = vec![vec![
        [0.0, 0.0],
        [10.0, 0.0],
        [10.0, 10.0],
        [0.0, 10.0],
    ]];
    let shape_a: Vec<Vec<Vec<[f64; 2]>>> = vec![poly_a];

    let poly_b: Vec<Vec<[f64; 2]>> = vec![vec![
        [5.0, 0.0],
        [15.0, 0.0],
        [15.0, 10.0],
        [5.0, 10.0],
    ]];
    let shape_b: Vec<Vec<Vec<[f64; 2]>>> = vec![poly_b];

    let overlay = FloatOverlay::with_subj_and_clip(&shape_a, &shape_b);
    let result = overlay.overlay(OverlayRule::Intersect, FillRule::NonZero);
    println!("Intersection result: {:?}", result);
    assert!(!result.is_empty());
}

#[test]
fn test_slice_watershed_real_data() {
    use i_overlay::float::overlay::FloatOverlay;
    use i_overlay::core::overlay_rule::OverlayRule;

    let test_file = "/tmp/test_geom.json";
    if !std::path::Path::new(test_file).exists() {
        return;
    }
    let data: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(test_file).unwrap()).unwrap();
    let res_lon = data["resolved"]["lon"].as_f64().unwrap();
    let res_lat = data["resolved"]["lat"].as_f64().unwrap();
    let res_pt = [res_lon, res_lat];

    let ws = &data["geometry"]["watershed"];
    let coords = ws["coordinates"].as_array().unwrap();

    let mut shapes: Vec<Vec<Vec<[f64; 2]>>> = Vec::new();
    for poly in coords {
        let mut rings: Vec<Vec<[f64; 2]>> = Vec::new();
        for ring in poly.as_array().unwrap() {
            let pts: Vec<[f64; 2]> = ring.as_array().unwrap().iter().map(|p| {
                [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()]
            }).collect();
            // i_overlay prefers open contours (without duplicate start/end point)
            let pts = if pts.len() > 1 && pts[0] == pts[pts.len() - 1] {
                pts[..pts.len() - 1].to_vec()
            } else {
                pts
            };
            rings.push(pts);
        }
        shapes.push(rings);
    }

    println!("Loaded shapes count: {}, total points in ring 0: {}", shapes.len(), shapes[0][0].len());

    let dx = -0.9470352291541994;
    let dy = -0.32112968523768265;
    let nx = -dy;
    let ny = dx;

    let span = 15.0;
    let upstream_span = 30.0;

    let p_left = [res_pt[0] - nx * span, res_pt[1] - ny * span];
    let p_right = [res_pt[0] + nx * span, res_pt[1] + ny * span];
    let p_back_right = [p_right[0] - dx * upstream_span, p_right[1] - dy * upstream_span];
    let p_back_left = [p_left[0] - dx * upstream_span, p_left[1] - dy * upstream_span];

    let cutter_poly = vec![p_left, p_right, p_back_right, p_back_left];
    let cutter_shape = vec![vec![cutter_poly]];

    println!("Cutter shape: {:?}", cutter_shape);

    let overlay = FloatOverlay::with_subj_and_clip(&shapes, &cutter_shape);
    let sliced_shapes = overlay.overlay(OverlayRule::Intersect, FillRule::NonZero);
    println!("Sliced shapes count: {}", sliced_shapes.len());
    if !sliced_shapes.is_empty() {
        println!("Sliced shape 0 outer ring pts: {}", sliced_shapes[0][0].len());
        // Find minimum distance from res_pt to any segment of the sliced shape
        let ring = &sliced_shapes[0][0];
        let mut min_dist_m = f64::INFINITY;
        for i in 0..ring.len() {
            let p1 = ring[i];
            let p2 = ring[(i + 1) % ring.len()];
            let dx = p2[0] - p1[0];
            let dy = p2[1] - p1[1];
            let len_sq = dx * dx + dy * dy;
            let t = if len_sq == 0.0 { 0.0 } else { (((res_pt[0] - p1[0]) * dx + (res_pt[1] - p1[1]) * dy) / len_sq).clamp(0.0, 1.0) };
            let proj = [p1[0] + t * dx, p1[1] + t * dy];
            let d_deg = (res_pt[0] - proj[0]).hypot(res_pt[1] - proj[1]);
            let d_m = d_deg * 111_000.0;
            if d_m < min_dist_m {
                min_dist_m = d_m;
            }
        }
        println!("Min distance from res_pt to sliced boundary: {:.4} meters", min_dist_m);
    }
}

#[test]
fn test_jaipur_clipping() {
    use i_overlay::float::overlay::FloatOverlay;
    use i_overlay::core::overlay_rule::OverlayRule;

    let url = "http://127.0.0.1:8787/api/delineate?lat=27.00&lon=75.80";
    let body = match std::process::Command::new("curl").arg("-s").arg(url).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(_) => return,
    };
    if body.is_empty() { return; }
    let data: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return,
    };

    let res_lon = data["resolved"]["lon"].as_f64().unwrap();
    let res_lat = data["resolved"]["lat"].as_f64().unwrap();
    let res_pt = [res_lon, res_lat];

    let ws = &data["geometry"]["watershed"];
    let coords = ws["coordinates"].as_array().unwrap();

    let mut shapes: Vec<Vec<Vec<[f64; 2]>>> = Vec::new();
    for poly in coords {
        let mut rings: Vec<Vec<[f64; 2]>> = Vec::new();
        for ring in poly.as_array().unwrap() {
            let mut pts: Vec<[f64; 2]> = ring.as_array().unwrap().iter().map(|p| {
                [p[0].as_f64().unwrap(), p[1].as_f64().unwrap()]
            }).collect();
            if pts.len() > 1 && pts[0] == pts[pts.len() - 1] {
                pts.pop();
            }
            rings.push(pts);
        }
        shapes.push(rings);
    }

    println!("Jaipur shapes count: {}, outer ring pts: {}", shapes.len(), shapes[0][0].len());

    let dx = -0.2951225930018088;
    let dy = -0.9554593947938806;
    let nx = -dy;
    let ny = dx;

    let span = 15.0;
    let upstream_span = 30.0;

    let p_left = [res_pt[0] - nx * span, res_pt[1] - ny * span];
    let p_right = [res_pt[0] + nx * span, res_pt[1] + ny * span];
    let p_back_right = [p_right[0] - dx * upstream_span, p_right[1] - dy * upstream_span];
    let p_back_left = [p_left[0] - dx * upstream_span, p_left[1] - dy * upstream_span];

    let cutter_poly = vec![p_left, p_right, p_back_right, p_back_left];
    let cutter_shape = vec![vec![cutter_poly]];

    let overlay = FloatOverlay::with_subj_and_clip(&shapes, &cutter_shape);
    let sliced_shapes = overlay.overlay(OverlayRule::Intersect, FillRule::NonZero);
    println!("Jaipur sliced shapes count: {}", sliced_shapes.len());
    if !sliced_shapes.is_empty() {
        println!("Jaipur sliced shape 0 outer ring pts: {}", sliced_shapes[0][0].len());
        let ring = &sliced_shapes[0][0];
        let mut min_dist_m = f64::INFINITY;
        for i in 0..ring.len() {
            let p1 = ring[i];
            let p2 = ring[(i + 1) % ring.len()];
            let dx = p2[0] - p1[0];
            let dy = p2[1] - p1[1];
            let len_sq = dx * dx + dy * dy;
            let t = if len_sq == 0.0 { 0.0 } else { (((res_pt[0] - p1[0]) * dx + (res_pt[1] - p1[1]) * dy) / len_sq).clamp(0.0, 1.0) };
            let proj = [p1[0] + t * dx, p1[1] + t * dy];
            let d_deg = (res_pt[0] - proj[0]).hypot(res_pt[1] - proj[1]);
            let d_m = d_deg * 111_000.0;
            if d_m < min_dist_m {
                min_dist_m = d_m;
            }
        }
        println!("Jaipur min distance from res_pt to sliced boundary: {:.4} meters", min_dist_m);
    }
}

#[test]
fn test_orig_geom_direction() {
    let url = "http://127.0.0.1:8787/api/delineate?lat=27.00&lon=75.80";
    let body = match std::process::Command::new("curl").arg("-s").arg(url).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).to_string(),
        Err(_) => return,
    };
    if body.is_empty() { return; }
    let data: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return,
    };

    let res_lon = data["resolved"]["lon"].as_f64().unwrap();
    let res_lat = data["resolved"]["lat"].as_f64().unwrap();
    let res_pt = [res_lon, res_lat];

    // Find closest reach in features
    let reaches = data["geometry"]["reaches"]["features"].as_array().unwrap();
    let mut best_dir = None;
    let mut best_dist_sq = f64::INFINITY;

    for r in reaches {
        let geom = &r["geometry"];
        let coords = geom["coordinates"].as_array().unwrap();
        for i in 0..(coords.len().saturating_sub(1)) {
            let p1 = [coords[i][0].as_f64().unwrap(), coords[i][1].as_f64().unwrap()];
            let p2 = [coords[i+1][0].as_f64().unwrap(), coords[i+1][1].as_f64().unwrap()];
            let dx = p2[0] - p1[0];
            let dy = p2[1] - p1[1];
            let mag = dx.hypot(dy);
            if mag < 1e-9 { continue; }
            let len_sq = dx*dx + dy*dy;
            let t = (((res_pt[0]-p1[0])*dx + (res_pt[1]-p1[1])*dy)/len_sq).clamp(0.0, 1.0);
            let proj = [p1[0] + t*dx, p1[1] + t*dy];
            let d = (res_pt[0]-proj[0]).powi(2) + (res_pt[1]-proj[1]).powi(2);
            if d < best_dist_sq {
                best_dist_sq = d;
                best_dir = Some((dx / mag, dy / mag));
            }
        }
    }

    println!("Best dir from reaches: {:?}", best_dir);
    assert!(best_dir.is_some());
    let (dx, dy) = best_dir.unwrap();
    println!("dx = {}, dy = {}", dx, dy);
}




