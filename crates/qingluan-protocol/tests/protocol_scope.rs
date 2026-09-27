#[test]
fn protocol_exposes_only_the_approved_s8_subset() {
    let proto = include_str!("../../../proto/qingluan/terminal/v1/terminal.proto");
    let methods = proto
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("rpc "))
        .collect::<Vec<_>>();
    assert_eq!(methods.len(), 11);
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
        "WatchSessionEvents",
        "AckSessionEvents",
    ] {
        assert!(
            methods
                .iter()
                .any(|line| line.starts_with(&format!("rpc {expected}(")))
        );
    }
    // §3: only WatchSessionEvents (and the later ObserveTerminal) stream.
    assert!(
        methods.contains(&"rpc WatchSessionEvents(WatchSessionEventsRequest)")
            && proto.contains("returns (stream WatchSessionEventsResponse)")
    );
    for deferred in [
        "ObserveTerminal",
        "ClearTerminalLogs",
        "DeleteTerminalRecord",
        "PruneSessionEvents",
    ] {
        assert!(!proto.contains(&format!("rpc {deferred}(")));
    }
}
