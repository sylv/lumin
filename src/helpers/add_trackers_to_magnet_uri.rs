const DEFAULT_TRACKERS: [&str; 10] = [
    "udp://tracker.coppersurfer.tk:6969/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://open.tracker.cl:1337/announce",
    "udp://explodie.org:6969/announce",
    "udp://tracker.leechers-paradise.org:6969/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://tracker-udp.gbitt.info:80/announce",
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://open.stealth.si:80/announce",
];

pub fn add_trackers_to_magnet_uri(magnet_uri: &str) -> String {
    let mut parsed_magnet = url::Url::parse(magnet_uri).expect("Invalid magnet URI");
    for tracker in DEFAULT_TRACKERS.iter() {
        parsed_magnet.query_pairs_mut().append_pair("tr", tracker);
    }

    parsed_magnet.to_string()
}
