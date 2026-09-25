//! Property coverage for the shared renderer-contract value types.
//!
//! `maverick-render` carries no algorithms — it is the vocabulary the GPU
//! backends and the compositor agree on — so what is testable here is the part
//! of the contract a backend is allowed to *rely* on: `Display` being total and
//! safe to splice into a fixed-shape startup report, and the zero values being
//! inert so an uninitialised quad cannot paint over the screen.
//!
//! The geometry arithmetic those types exist for (edge derivation, scissor
//! flipping, aspect correction) lives in the consuming backends, not here, so
//! there is deliberately no property about it in this crate.

use maverick_render::{Acceleration, DrawQuad, RendererInfo};
use proptest::prelude::*;

/// Driver strings come from `GLXQueryServerString` / `VkPhysicalDeviceProperties`
/// and are entirely untrusted: they may be empty, contain newlines, or carry
/// control bytes from a broken driver.
fn driver_string() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[ -~]{1,24}",
        "[\\n\\t\\r ]{1,8}",
        ".*",
    ]
}

fn accel() -> impl Strategy<Value = Acceleration> {
    prop_oneof![
        Just(Acceleration::Gpu),
        Just(Acceleration::Software),
        Just(Acceleration::Unknown),
    ]
}

proptest! {
    /// The acceleration label is spliced into the startup report as its last
    /// field, so it has to be a single line of plain text: a label carrying a
    /// newline or a control byte would break the report into more (or fewer)
    /// lines than a log reader expects and lose the field boundaries. The three
    /// variants must also spell themselves differently, otherwise a log line
    /// cannot tell a real GPU from a software fallback.
    #[test]
    fn acceleration_label_is_one_plain_line_and_unique(
        a in accel(),
    ) {
        let label = a.to_string();
        prop_assert!(!label.is_empty(), "an empty label loses the field entirely");
        prop_assert!(
            label.chars().all(|c| !c.is_control()),
            "label {:?} would break the fixed-shape report",
            label
        );
        for other in [Acceleration::Gpu, Acceleration::Software, Acceleration::Unknown] {
            if other != a {
                prop_assert_ne!(
                    &label,
                    &other.to_string(),
                    "two acceleration variants share one label: {:?}",
                    label
                );
            }
        }
    }

    /// The startup report is what a user greps when the compositor misbehaves,
    /// so it must carry every field the caller passed — verbatim, hostile
    /// driver strings included — under its own label, in the documented order,
    /// with the acceleration verdict last. Total for any input: `Display` on
    /// this type is called during start-up, where a panic would take the window
    /// manager down before it ever drew.
    #[test]
    fn renderer_info_report_carries_every_field(
        vendor in driver_string(),
        renderer in driver_string(),
        version in driver_string(),
        a in accel(),
    ) {
        let info = RendererInfo {
            backend: "OpenGL/GLX",
            vendor: vendor.clone(),
            renderer: renderer.clone(),
            version: version.clone(),
            accelerated: a,
        };
        let report = info.to_string();

        prop_assert!(
            report.starts_with("Compositor:\n"),
            "the report must open with its header: {:?}",
            report
        );
        // Each labelled field appears after the previous one, so a rename, a
        // reorder or a dropped field is caught even when a hostile field value
        // happens to contain a label verbatim.
        let mut cursor = 0usize;
        for label in ["  Backend: ", "  Vendor: ", "  Renderer: ", "  Version: ", "  Acceleration: "] {
            let at = report[cursor..]
                .find(label)
                .map(|i| cursor + i)
                .unwrap_or_else(|| panic!("report is missing the {} field: {:?}", label, report));
            cursor = at + label.len();
        }
        for value in [info.backend, info.vendor.as_str(), info.renderer.as_str(), info.version.as_str()] {
            prop_assert!(
                report.contains(value),
                "field value {:?} is missing from the report: {:?}",
                value,
                report
            );
        }
        prop_assert_eq!(
            report.lines().last().unwrap_or_default(),
            format!("  Acceleration: {}", a),
            "the acceleration verdict is the report's closing line"
        );
    }
}

/// A quad built with `..Default::default()` is the shape a backend ends up with
/// when it has nothing to draw but still walks its draw path. It must be inert:
/// zero-area destination, no rounding, and `opacity == 0.0`, which the type
/// documents as fully transparent. A default quad that painted an opaque
/// rectangle would blank the screen on every backend that defaulted one.
#[test]
fn default_draw_quad_draws_nothing() {
    let q = DrawQuad::default();
    assert_eq!(
        (q.dst[0], q.dst[1]),
        (q.dst[2], q.dst[3]),
        "a default destination must have zero area, got {:?}",
        q.dst
    );
    assert_eq!(q.src, [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(q.size, [0.0, 0.0]);
    assert_eq!(q.radius, 0.0);
    assert_eq!(
        q.opacity, 0.0,
        "opacity 0.0 is the documented transparent end of the range"
    );
}
