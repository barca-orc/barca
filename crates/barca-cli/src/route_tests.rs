use super::*;
use clap::CommandFactory;

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

#[test]
fn local_commands_refuse_server_and_local() {
    for line in [
        "barca serve p.py --server",
        "barca plan --server=http://h",
        "barca plan p.py --local",
        "barca docs --server",
        "barca version --server",
    ] {
        let err = refuse_on_local_commands(&argv(line)).expect(line);
        assert_eq!(err.kind, ErrorKind::Usage, "{line}");
        assert!(err.error.contains("always runs on this machine"), "{line}");
    }
    for line in [
        "barca get x --server",
        "barca status --local",
        "barca sql q --local",
        "barca sql q --server",
        "barca p.py --server",
        "barca serve p.py",
    ] {
        assert!(refuse_on_local_commands(&argv(line)).is_none(), "{line}");
    }
}
