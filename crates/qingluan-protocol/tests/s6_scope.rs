#[test]
fn s6_protocol_exposes_only_the_approved_unary_subset() {
    let proto = include_str!("../../../proto/qingluan/terminal/v1/terminal.proto");
    let methods = proto
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("rpc "))
        .collect::<Vec<_>>();
    assert_eq!(methods.len(), 9);
    for expected in [
        "GetServerInfo",
        "AcquireControl",
        "RenewControl",
        "ReleaseControl",
        "Start",
        "Send",
        "Stop",
        "Read",
        "Tail",
    ] {
        assert!(
            methods
                .iter()
                .any(|line| line.starts_with(&format!("rpc {expected}(")))
        );
    }
    for deferred in [
        "WatchSessionEvents",
        "AckSessionEvents",
        "ObserveTerminal",
        "ClearTerminalLogs",
        "DeleteTerminalRecord",
        "PruneSessionEvents",
    ] {
        assert!(!proto.contains(&format!("rpc {deferred}(")));
    }
}
