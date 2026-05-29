use peekaboo::trace::network::{parse_lsof, diff_connections, Connection};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[test]
fn parses_lsof_sample() {
    let raw = include_str!("fixtures/lsof_sample.txt");
    let conns = parse_lsof(raw);
    assert_eq!(conns.len(), 3);
    assert!(conns.iter().any(|c| c.pid == 4719 && c.remote.port() == 443));
}

#[test]
fn diff_emits_opens_for_new_conns() {
    let prev: Vec<Connection> = vec![];
    let now = vec![Connection {
        pid: 4719,
        local: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192,168,1,5)), 55321),
        remote: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(54,230,93,21)), 443),
        state: "ESTABLISHED".into(),
    }];
    let (opens, closes) = diff_connections(&prev, &now);
    assert_eq!(opens.len(), 1);
    assert_eq!(closes.len(), 0);
}

#[test]
fn diff_emits_closes_for_gone_conns() {
    let prev = vec![Connection {
        pid: 4719,
        local: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192,168,1,5)), 55321),
        remote: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(54,230,93,21)), 443),
        state: "ESTABLISHED".into(),
    }];
    let now: Vec<Connection> = vec![];
    let (opens, closes) = diff_connections(&prev, &now);
    assert_eq!(opens.len(), 0);
    assert_eq!(closes.len(), 1);
}
