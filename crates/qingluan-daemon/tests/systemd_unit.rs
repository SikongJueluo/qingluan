#[test]
fn nix_user_units_require_cgroup_delegation_and_private_creation_mode() {
    let home = include_str!("../../../nix/modules/qingluan-home.nix");
    let system = include_str!("../../../nix/modules/qingluan-system.nix");
    for module in [home, system] {
        assert!(module.contains("Delegate = true;"));
        assert!(module.contains("UMask = \"0077\";"));
        assert!(module.contains("RuntimeDirectoryMode = \"0700\";"));
        assert!(module.contains("TimeoutStopSec = 120;"));
        assert!(module.contains("/bin/qingluan daemon start"));
    }
}
