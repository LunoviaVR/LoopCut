//! `.cube` parsing and the CPU reference sampler. These tests need no GPU.
//!
//! Run with `cargo test -p loopcut-render --test lut`.

use std::fmt::Write as _;

use loopcut_render::lut::{LutError, MAX_LUT_FILE_BYTES, load_cube, parse_cube};

/// Builds the text of a 3D `.cube` table of `size` points per axis. `f` maps each grid coordinate in [0, 1] to output
/// RGB, and red varies fastest in the rows.
fn cube_3d(size: usize, title: Option<&str>, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
    let mut text = String::new();
    if let Some(title) = title {
        writeln!(text, "TITLE \"{title}\"").expect("write to string");
    }
    writeln!(text, "# generated for a test").expect("write to string");
    writeln!(text, "LUT_3D_SIZE {size}").expect("write to string");
    let scale = (size - 1) as f32;
    for b in 0..size {
        for g in 0..size {
            for r in 0..size {
                let rgb = f([r as f32 / scale, g as f32 / scale, b as f32 / scale]);
                writeln!(text, "{:.7} {:.7} {:.7}", rgb[0], rgb[1], rgb[2])
                    .expect("write to string");
            }
        }
    }
    text
}

/// Builds the text of a 1D `.cube` table with `size` entries. `f` maps each input in [0, 1] to output RGB.
fn cube_1d(size: usize, f: impl Fn(f32) -> [f32; 3]) -> String {
    let mut text = format!("LUT_1D_SIZE {size}\n");
    let scale = (size - 1) as f32;
    for i in 0..size {
        let rgb = f(i as f32 / scale);
        writeln!(text, "{:.7} {:.7} {:.7}", rgb[0], rgb[1], rgb[2]).expect("write to string");
    }
    text
}

/// A deterministic spread of points in [0, 1]^3, including the corners and the faces.
fn probe_points() -> Vec<[f32; 3]> {
    let mut points = Vec::new();
    let steps = [0.0_f32, 0.07, 0.25, 0.42, 0.5, 0.63, 0.81, 0.97, 1.0];
    for &r in &steps {
        for &g in &steps {
            for &b in &steps {
                points.push([r, g, b]);
            }
        }
    }
    points
}

fn max_channel_error(actual: [f32; 3], expected: [f32; 3]) -> f32 {
    (0..3)
        .map(|c| (actual[c] - expected[c]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn identity_3d_table_is_a_no_op_within_one_level() {
    for size in [2_usize, 9, 17] {
        let lut = parse_cube(&cube_3d(size, Some("identity"), |p| p)).expect("parse");
        assert!(lut.is_3d());
        assert_eq!(lut.size(), size as u32);
        assert_eq!(lut.title(), Some("identity"));
        for point in probe_points() {
            let out = lut.sample(point);
            assert!(
                max_channel_error(out, point) <= 1.0 / 255.0,
                "size {size}: {point:?} -> {out:?}"
            );
        }
    }
}

#[test]
fn inverting_3d_table_inverts() {
    let lut =
        parse_cube(&cube_3d(17, None, |p| [1.0 - p[0], 1.0 - p[1], 1.0 - p[2]])).expect("parse");
    assert!(lut.title().is_none());
    for point in probe_points() {
        let out = lut.sample(point);
        let expected = [1.0 - point[0], 1.0 - point[1], 1.0 - point[2]];
        assert!(
            max_channel_error(out, expected) <= 1.0 / 255.0,
            "{point:?} -> {out:?}"
        );
    }
}

#[test]
fn tetrahedral_interpolation_is_exact_for_a_linear_table() {
    // A table that is linear in every channel (a per-channel gain and offset) must come back exactly from the
    // interpolation, whichever tetrahedron a point falls in.
    let lut = parse_cube(&cube_3d(5, None, |p| {
        [0.8 * p[0] + 0.1, 0.5 * p[1], 0.2 + 0.6 * p[2]]
    }))
    .expect("parse");
    for point in probe_points() {
        let expected = [0.8 * point[0] + 0.1, 0.5 * point[1], 0.2 + 0.6 * point[2]];
        assert!(
            max_channel_error(lut.sample(point), expected) < 1e-5,
            "{point:?}"
        );
    }
}

#[test]
fn input_outside_the_domain_clamps_to_it() {
    let text = "DOMAIN_MIN 0.25 0.25 0.25\nDOMAIN_MAX 0.75 0.75 0.75\n".to_owned()
        + &cube_3d(2, None, |p| p);
    let lut = parse_cube(&text).expect("parse");
    assert_eq!(lut.domain_min(), [0.25; 3]);
    assert_eq!(lut.domain_max(), [0.75; 3]);
    // The table is the grid itself, so over the domain [0.25, 0.75] an input maps to (input - 0.25) / 0.5. Inputs
    // below the domain clamp to the first entry (0) and inputs above it to the last (1).
    let below = lut.sample([0.0, 0.5, 1.0]);
    assert!(below[0].abs() < 1e-6, "below the domain: {below:?}");
    assert!((below[2] - 1.0).abs() < 1e-6, "above the domain: {below:?}");
    let middle = lut.sample([0.5, 0.5, 0.5]);
    assert!(max_channel_error(middle, [0.5; 3]) < 1e-6, "{middle:?}");
}

#[test]
fn identity_and_inverting_1d_tables() {
    let identity = parse_cube(&cube_1d(256, |x| [x, x, x])).expect("parse");
    assert!(!identity.is_3d());
    assert_eq!(identity.size(), 256);
    for point in probe_points() {
        assert!(max_channel_error(identity.sample(point), point) <= 1.0 / 255.0);
    }
    let inverting = parse_cube(&cube_1d(64, |x| [1.0 - x, 1.0 - x, 1.0 - x])).expect("parse");
    for point in probe_points() {
        let expected = point.map(|c| 1.0 - c);
        assert!(max_channel_error(inverting.sample(point), expected) <= 1.0 / 255.0);
    }
}

#[test]
fn one_dimensional_table_reads_each_channel_from_its_own_column() {
    // Output red depends on input only, and green and blue are fixed: the columns are independent.
    let text = "LUT_1D_SIZE 2\n0 0.5 1\n1 0.25 0\n";
    let lut = parse_cube(text).expect("parse");
    let out = lut.sample([0.5, 0.5, 0.5]);
    assert!((out[0] - 0.5).abs() < 1e-6);
    assert!((out[1] - 0.375).abs() < 1e-6);
    assert!((out[2] - 0.5).abs() < 1e-6);
}

#[test]
fn comments_blank_lines_bom_and_trailing_comments_are_ignored() {
    let text = "\u{feff}# header\n\n  TITLE \"spaced\"   \nLUT_1D_SIZE 2 # two entries\n\n0 0 0 # black\n1 1 1\n";
    let lut = parse_cube(text).expect("parse");
    assert_eq!(lut.title(), Some("spaced"));
    assert_eq!(lut.size(), 2);
}

#[test]
fn iridas_input_range_sets_the_domain() {
    let text = "LUT_3D_INPUT_RANGE 0.0 0.5\n".to_owned() + &cube_3d(2, None, |p| p);
    let lut = parse_cube(&text).expect("parse");
    assert_eq!(lut.domain_max(), [0.5; 3]);
}

fn syntax_line(text: &str) -> usize {
    match parse_cube(text) {
        Err(LutError::Syntax { line, .. }) => line,
        other => panic!("expected a syntax error, got {other:?}"),
    }
}

#[test]
fn malformed_rows_report_their_line_number() {
    // Line 3 has two values instead of three.
    let text = "LUT_1D_SIZE 2\n0 0 0\n1 1\n";
    assert_eq!(syntax_line(text), 3);
    // A non-number on line 2.
    assert_eq!(syntax_line("LUT_1D_SIZE 2\nzero 0 0\n1 1 1\n"), 2);
    // NaN and infinity are not allowed.
    assert_eq!(syntax_line("LUT_1D_SIZE 2\nnan 0 0\n1 1 1\n"), 2);
    assert_eq!(syntax_line("LUT_1D_SIZE 2\n0 inf 0\n1 1 1\n"), 2);
    // A data row before any size keyword.
    assert_eq!(syntax_line("0 0 0\nLUT_1D_SIZE 2\n"), 1);
    // A keyword after the data.
    assert_eq!(
        syntax_line("LUT_1D_SIZE 2\n0 0 0\nTITLE \"late\"\n1 1 1\n"),
        3
    );
    // Too many rows: the third data row (line 4) is beyond the two that a 2-entry table allows.
    assert_eq!(
        syntax_line("LUT_1D_SIZE 2\n0 0 0\n0.5 0.5 0.5\n1 1 1\n0 0 0\n"),
        4
    );
}

#[test]
fn unknown_keywords_and_duplicate_sizes_are_rejected() {
    assert_eq!(syntax_line("LUT_3D_SIZE 2\nFOO 1\n"), 2);
    assert_eq!(syntax_line("LUT_1D_SIZE 2\nLUT_3D_SIZE 2\n"), 2);
    assert_eq!(syntax_line("LUT_1D_SIZE 2\nLUT_1D_SIZE 2\n"), 2);
}

#[test]
fn size_must_be_an_integer_in_range() {
    assert_eq!(syntax_line("LUT_3D_SIZE 1\n"), 1);
    assert_eq!(syntax_line("LUT_3D_SIZE 257\n"), 1);
    assert_eq!(syntax_line("LUT_3D_SIZE 2.5\n"), 1);
    assert_eq!(syntax_line("LUT_1D_SIZE 0\n"), 1);
    assert_eq!(syntax_line("LUT_1D_SIZE 70000\n"), 1);
    // A 256-cube is the largest accepted 3D size; a short file then fails on the row count, not the size.
    match parse_cube("LUT_3D_SIZE 256\n0 0 0\n") {
        Err(LutError::Invalid(message)) => assert!(message.contains("16777216"), "{message}"),
        other => panic!("expected a row-count error, got {other:?}"),
    }
}

#[test]
fn file_level_problems_are_invalid_not_syntax_errors() {
    assert!(matches!(
        parse_cube("0 0 0\n"),
        Err(LutError::Syntax { .. })
    ));
    assert!(matches!(
        parse_cube("# nothing here\n"),
        Err(LutError::Invalid(_))
    ));
    // Two rows for a 2-entry 1D table is right, but one row is not enough.
    assert!(matches!(
        parse_cube("LUT_1D_SIZE 2\n0 0 0\n"),
        Err(LutError::Invalid(_))
    ));
    // A 2-cube needs eight rows.
    assert!(matches!(
        parse_cube("LUT_3D_SIZE 2\n0 0 0\n1 1 1\n"),
        Err(LutError::Invalid(_))
    ));
    // A domain whose minimum is not below its maximum is rejected.
    assert!(matches!(
        parse_cube(&("DOMAIN_MIN 1 0 0\n".to_owned() + &cube_3d(2, None, |p| p))),
        Err(LutError::Invalid(_))
    ));
    assert!(matches!(
        parse_cube(&("DOMAIN_MAX 0 1 1\n".to_owned() + &cube_3d(2, None, |p| p))),
        Err(LutError::Invalid(_))
    ));
}

#[test]
fn load_cube_reads_a_file_and_reports_a_missing_one() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("create the temp directory");
    let path = dir.join("loopcut-lut-identity.cube");
    std::fs::write(&path, cube_3d(3, Some("disk"), |p| p)).expect("write the test LUT");
    let lut = load_cube(&path).expect("load");
    assert_eq!(lut.title(), Some("disk"));
    assert_eq!(lut.size(), 3);

    let missing = dir.join("loopcut-lut-does-not-exist.cube");
    match load_cube(&missing) {
        Err(LutError::Read { path, .. }) => assert!(path.contains("does-not-exist")),
        other => panic!("expected a read error, got {other:?}"),
    }
}

#[test]
fn a_lut_clone_shares_its_table() {
    let lut = parse_cube(&cube_3d(2, None, |p| p)).expect("parse");
    let copy = lut.clone();
    assert_eq!(lut.sample([0.3, 0.6, 0.9]), copy.sample([0.3, 0.6, 0.9]));
    // Two parses are distinct tables, even with identical text.
    let other = parse_cube(&cube_3d(2, None, |p| p)).expect("parse");
    assert_eq!(other.size(), lut.size());
}

#[test]
fn title_values_may_contain_hash_signs() {
    let lut = parse_cube("TITLE \"Look #3 # warm\"\nLUT_1D_SIZE 2\n0 0 0\n1 1 1\n").expect("parse");
    assert_eq!(lut.title(), Some("Look #3 # warm"));
    let bare = parse_cube("TITLE Teal#2\nLUT_1D_SIZE 2\n0 0 0\n1 1 1\n").expect("parse");
    assert_eq!(bare.title(), Some("Teal#2"));
}

#[test]
fn load_cube_streams_crlf_lines_and_a_byte_order_mark() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("create the temp directory");
    let path = dir.join("loopcut-lut-crlf.cube");
    std::fs::write(
        &path,
        "\u{feff}TITLE \"crlf\"\r\nLUT_1D_SIZE 2\r\n0 0 0 # black\r\n1 1 1\r\n",
    )
    .expect("write the test LUT");
    let from_disk = load_cube(&path).expect("load");
    let from_text =
        parse_cube("TITLE \"crlf\"\nLUT_1D_SIZE 2\n0 0 0 # black\n1 1 1\n").expect("parse");
    assert_eq!(from_disk.title(), Some("crlf"));
    assert_eq!(from_disk.size(), 2);
    for point in [[0.0, 0.0, 0.0], [0.25, 0.5, 0.75], [1.0, 1.0, 1.0]] {
        assert_eq!(from_disk.sample(point), from_text.sample(point));
    }
}

#[test]
fn load_cube_refuses_a_directory() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("create the temp directory");
    match load_cube(&dir) {
        Err(LutError::Read { message, .. }) => {
            assert!(message.contains("regular file"), "message: {message}");
        }
        other => panic!("expected a read error for a directory, got {other:?}"),
    }
}

#[test]
fn oversized_files_are_refused_before_they_are_read() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("create the temp directory");
    let path = dir.join("loopcut-lut-oversized.cube");
    let file = std::fs::File::create(&path).expect("create");
    file.set_len(MAX_LUT_FILE_BYTES + 1)
        .expect("set the length");
    drop(file);
    match load_cube(&path) {
        Err(LutError::TooLarge { size, limit }) => {
            assert_eq!(size, MAX_LUT_FILE_BYTES + 1);
            assert_eq!(limit, MAX_LUT_FILE_BYTES);
        }
        other => panic!("expected TooLarge, got {other:?}"),
    }
}

#[test]
fn debug_output_shows_the_header_and_never_the_table() {
    let lut = parse_cube(&cube_3d(3, Some("dbg"), |p| p)).expect("parse");
    let text = format!("{lut:?}");
    assert!(text.contains("entries: 27"), "{text}");
    assert!(text.contains("title: Some(\"dbg\")"), "{text}");
    assert!(text.len() < 300, "debug output is {} bytes", text.len());
}
