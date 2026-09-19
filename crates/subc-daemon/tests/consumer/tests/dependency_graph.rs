use std::process::Command;

#[test]
fn consumer_dependency_graph_excludes_core() {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--locked",
            "-p",
            "subc-daemon-consumer",
            "--prefix",
            "none",
        ])
        .output()
        .expect("run cargo tree for the consumer fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).unwrap();
    let daemon_count = tree
        .lines()
        .filter(|line| line.starts_with("subc-daemon v"))
        .count();
    let core_count = tree
        .lines()
        .filter(|line| line.starts_with("subc-core v"))
        .count();
    assert!(daemon_count > 0, "positive control missing: {tree}");
    assert_eq!(
        core_count, 0,
        "consumer must not depend on subc-core: {tree}"
    );
    println!("dependency counts: subc-daemon={daemon_count}, subc-core={core_count}");
}
