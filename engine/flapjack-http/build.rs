use std::path::Path;

#[path = "src/dashboard_build.rs"]
mod dashboard_build;

const DASHBOARD_DIST_DIR: &str = "../dashboard/dist";
const DASHBOARD_ASSETS_DIR: &str = "../dashboard/dist/assets";

fn main() {
    // Keep cfg validation explicit so unknown cfgs fail fast.
    println!("cargo:rustc-check-cfg=cfg(flapjack_dashboard_dist)");
    println!("cargo:rerun-if-env-changed=FLAPJACK_REQUIRE_DASHBOARD");
    println!("cargo:rerun-if-changed={DASHBOARD_DIST_DIR}");
    println!("cargo:rerun-if-changed={DASHBOARD_DIST_DIR}/index.html");
    println!("cargo:rerun-if-changed={DASHBOARD_ASSETS_DIR}");

    let dashboard_dist = Path::new(DASHBOARD_DIST_DIR);
    if dashboard_dist.is_dir() {
        println!("cargo:rustc-cfg=flapjack_dashboard_dist");
    }

    if require_dashboard_assets()
        && !dashboard_build::dashboard_dist_has_real_assets(dashboard_dist)
    {
        // A cargo feature is also avoided: release.yml passes --no-default-features, disabling default features.
        panic!(
            "FLAPJACK_REQUIRE_DASHBOARD requires real dashboard assets in {DASHBOARD_ASSETS_DIR}"
        );
    }
}

fn require_dashboard_assets() -> bool {
    std::env::var("FLAPJACK_REQUIRE_DASHBOARD")
        .map(|value| {
            value == "1"
                || value.eq_ignore_ascii_case("true")
                || value.eq_ignore_ascii_case("yes")
                || value.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false)
}
