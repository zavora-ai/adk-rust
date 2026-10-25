//! Properties of the tray's guards (spec 3.3 §5.5): only this computer is reached, only the
//! kernel's own `/ui/` pages stay in the shell's windows, and the panel stays on screen.

use std::net::{Ipv4Addr, Ipv6Addr};

use adk_desktop_core::{Point, Rect, Route, ServerTarget, Size, place_panel};
use proptest::prelude::*;
use url::Url;

fn config() -> ProptestConfig {
    ProptestConfig { cases: 256, ..ProptestConfig::default() }
}

/// An IPv4 address outside 127.0.0.0/8 and other than 0.0.0.0.
fn remote_v4() -> impl Strategy<Value = Ipv4Addr> {
    any::<u32>()
        .prop_map(Ipv4Addr::from)
        .prop_filter("not this computer", |ip| !ip.is_loopback() && !ip.is_unspecified())
}

/// An IPv6 address other than `::1` and `::`.
fn remote_v6() -> impl Strategy<Value = Ipv6Addr> {
    any::<u128>()
        .prop_map(Ipv6Addr::from)
        .prop_filter("not this computer", |ip| !ip.is_loopback() && !ip.is_unspecified())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn another_machine_is_always_refused(v4 in remote_v4(), v6 in remote_v6(), name in "[a-z]{1,12}(\\.[a-z]{2,6}){0,2}", port in 1_u16..) {
        for server in [format!("http://{v4}:{port}"), format!("http://[{v6}]:{port}")] {
            prop_assert!(ServerTarget::resolve(Some(&server), None, None).is_err(), "{}", server);
        }
        if name != "localhost" {
            let server = format!("http://{name}:{port}");
            prop_assert!(ServerTarget::resolve(Some(&server), None, None).is_err(), "{}", server);
        }
    }

    #[test]
    fn this_computer_is_always_accepted(a in any::<u8>(), b in any::<u8>(), c in any::<u8>(), port in 1_u16..) {
        for server in [
            format!("http://127.{a}.{b}.{c}:{port}"),
            format!("http://[::1]:{port}"),
            format!("http://localhost:{port}"),
            format!("127.{a}.{b}.{c}:{port}"),
        ] {
            let target = ServerTarget::resolve(Some(&server), None, None);
            prop_assert!(target.is_ok(), "{}: {:?}", server, target);
            prop_assert_eq!(target.unwrap().origin().port_or_known_default(), Some(port));
        }
    }

    #[test]
    fn no_other_origin_or_path_is_ever_the_panel_or_the_home(
        port in 1_u16..,
        other in 1_u16..,
        host in prop::sample::select(vec!["127.0.0.1", "localhost", "example.com", "[::1]", "10.0.0.2"]),
        scheme in prop::sample::select(vec!["http", "https"]),
        path in "/[a-z/]{0,12}",
    ) {
        let server = ServerTarget::resolve(Some(&format!("http://127.0.0.1:{port}")), None, None).unwrap();
        let elsewhere = Url::parse(&format!("{scheme}://{host}:{other}/ui/tray")).unwrap();
        if !server.same_origin(&elsewhere) {
            prop_assert!(!matches!(Route::classify(&elsewhere, &server), Route::Panel | Route::Home));
        }
        let mine = Url::parse(&format!("http://127.0.0.1:{port}{path}")).unwrap();
        if mine.path() != "/ui" && !mine.path().starts_with("/ui/") {
            prop_assert_eq!(Route::classify(&mine, &server), Route::External);
        }
    }

    #[test]
    fn the_panel_lies_inside_the_work_area(
        x in -4_000_i32..4_000,
        y in -2_000_i32..2_000,
        width in 400_u32..4_000,
        height in 400_u32..3_000,
        panel_width in 1_u32..400,
        panel_height in 1_u32..400,
        icon in prop::option::of((-5_000_i32..9_000, -3_000_i32..5_000, 1_u32..64, 1_u32..64)),
    ) {
        let work_area = Rect { origin: Point { x, y }, size: Size { width, height } };
        let size = Size { width: panel_width, height: panel_height };
        let tray = icon.map(|(x, y, width, height)| Rect { origin: Point { x, y }, size: Size { width, height } });
        let at = place_panel(tray, work_area, size);
        prop_assert!(at.x >= x && i64::from(at.x) + i64::from(panel_width) <= i64::from(x) + i64::from(width));
        prop_assert!(at.y >= y && i64::from(at.y) + i64::from(panel_height) <= i64::from(y) + i64::from(height));
    }
}
