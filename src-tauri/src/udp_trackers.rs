//! Removes UDP trackers from torrents before they're added.
//!
//! librqbit can't send UDP through the SOCKS5 proxy, so it would announce to `udp://` trackers
//! directly, from the real IP. Kept in this one file so it can be deleted once UDP goes through
//! the VPN: remove the file, its `mod` line in `main.rs`, and the `strip` call in
//! `torrents::add`.

use anyhow::{bail, Context, Result};
use librqbit::{torrent_from_bytes, AddTorrent, Magnet};

const NO_TRACKERS: &str = "this torrent has no HTTP(S) trackers. UDP trackers and DHT would \
    bypass the VPN, so they're disabled, which leaves no way to find peers";

/// Remove UDP trackers from a magnet link or .torrent file. Refuses a torrent left with no
/// HTTP(S) tracker, and torrents given by any other URL: librqbit would fetch that .torrent file
/// itself, after this check.
pub fn strip(add: AddTorrent<'_>) -> Result<AddTorrent<'static>> {
    match add {
        AddTorrent::Url(url) if url.starts_with("magnet:") => {
            Ok(AddTorrent::from_url(strip_magnet(&url)?))
        }
        AddTorrent::Url(_) => bail!("only magnet links and .torrent files can be added"),
        AddTorrent::TorrentFileBytes(bytes) => {
            Ok(AddTorrent::from_bytes(strip_torrent_file(&bytes)?))
        }
    }
}

/// Trackers librqbit can reach through the SOCKS5 proxy.
fn is_proxied(tracker: &str) -> bool {
    tracker.starts_with("http://") || tracker.starts_with("https://")
}

/// Rebuild a magnet link with only HTTP(S) trackers.
fn strip_magnet(magnet: &str) -> Result<String> {
    let parsed = Magnet::parse(magnet).context("not a valid magnet link")?;
    let info_hash = parsed
        .as_id20()
        .context("only BitTorrent v1 magnet links are supported")?;
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("xt", &format!("urn:btih:{}", info_hash.as_string()));
    if let Some(name) = &parsed.name {
        query.append_pair("dn", name);
    }
    let trackers: Vec<_> = parsed.trackers.iter().filter(|t| is_proxied(t)).collect();
    if trackers.is_empty() {
        bail!(NO_TRACKERS);
    }
    for tracker in trackers {
        query.append_pair("tr", tracker);
    }
    Ok(format!("magnet:?{}", query.finish()))
}

/// Rebuild a .torrent file with only HTTP(S) trackers. The `info` dictionary is copied byte for
/// byte, so the info hash can't change. Other top-level keys (comment, web seeds, ...) are
/// dropped.
fn strip_torrent_file(bytes: &[u8]) -> Result<Vec<u8>> {
    let parsed = torrent_from_bytes(bytes).context("not a valid .torrent file")?;
    let mut trackers: Vec<&str> = parsed
        .iter_announce()
        .filter_map(|t| std::str::from_utf8(t.0).ok())
        .filter(|t| is_proxied(t))
        .collect();
    trackers.dedup();
    if trackers.is_empty() {
        bail!(NO_TRACKERS);
    }

    // Bencode: keys in sorted order, "announce-list" < "info"; one tracker per tier.
    let mut out = b"d13:announce-listl".to_vec();
    for tracker in trackers {
        out.extend_from_slice(format!("l{}:{}e", tracker.len(), tracker).as_bytes());
    }
    out.extend_from_slice(b"e4:info");
    out.extend_from_slice(parsed.info.raw_bytes.0);
    out.push(b'e');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magnet_keeps_only_http_trackers() {
        let magnet =
            "magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c&dn=Big+Buck+Bunny\
            &tr=udp%3A%2F%2Fexplodie.org%3A6969&tr=https%3A%2F%2Ftracker.example%2Fannounce";
        let clean = Magnet::parse(&strip_magnet(magnet).unwrap()).unwrap();
        assert_eq!(
            clean.as_id20().unwrap().as_string(),
            "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"
        );
        assert_eq!(clean.name.as_deref(), Some("Big Buck Bunny"));
        assert_eq!(clean.trackers, ["https://tracker.example/announce"]);
    }

    #[test]
    fn magnet_with_only_udp_trackers_is_refused() {
        let magnet = "magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c\
            &tr=udp%3A%2F%2Fexplodie.org%3A6969";
        assert!(strip_magnet(magnet).is_err());
    }

    #[test]
    fn torrent_file_keeps_info_hash_and_drops_udp_trackers() {
        let info =
            b"d6:lengthi5e4:name5:a.txt12:piece lengthi16384e6:pieces20:aaaaaaaaaaaaaaaaaaaae";
        let mut file = b"d8:announce22:udp://bad.example:69697:comment2:hi13:announce-listll22:udp://bad.example:6969el29:http://good.example/announce1ee4:info".to_vec();
        file.extend_from_slice(info);
        file.push(b'e');

        let original = torrent_from_bytes(&file).unwrap();
        let clean_bytes = strip_torrent_file(&file).unwrap();
        let clean = torrent_from_bytes(&clean_bytes).unwrap();
        assert_eq!(clean.info_hash, original.info_hash);
        let trackers: Vec<&[u8]> = clean.iter_announce().map(|t| t.0).collect();
        assert_eq!(trackers, [b"http://good.example/announce1".as_slice()]);
    }

    #[test]
    fn other_urls_are_refused() {
        let add = AddTorrent::from_url("https://example.com/file.torrent");
        assert!(strip(add).is_err());
    }
}
