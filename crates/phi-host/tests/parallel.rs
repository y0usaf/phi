#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use phi_host::{Host, HostConfig};

#[test]
fn public_parallel_example_completes_in_completion_order() {
    let host = Host::new(HostConfig::default()).expect("host");
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/extensions/parallel-demo.lua"
    ))
    .expect("example");
    host.load("examples/extensions/parallel-demo.lua", &source)
        .expect("load");
    let value = host
        .call_command("parallel-demo", "")
        .expect("call")
        .expect("value");
    assert_eq!(value["first"], "fast");
    assert_eq!(value["second"], "slow");
    assert_eq!(value["firstIndex"], 2);
}
