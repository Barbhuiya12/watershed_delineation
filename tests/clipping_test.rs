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
