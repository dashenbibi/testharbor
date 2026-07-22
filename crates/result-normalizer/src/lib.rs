//! Result normalization: Go test JSON + inventory helpers.

mod go_json;
mod inventory;
mod junit_xml;
mod model;

pub use go_json::{parse_go_test_json, parse_go_test_json_with_exit};
pub use inventory::{
    collect_go_inventory, intended_manifest_hash, inventory_hash, make_intended_manifest,
    observe_intended, IntendedObservation, IntendedTestManifest, InventoryError, TestIdentity,
    TestInventory,
};
pub use junit_xml::parse_junit_xml_reports_with_exit;
pub use model::*;
