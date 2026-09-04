//! Decrypt and dump a stored session (a base64 data file + a hex key) for
//! verification. Usage: `dump_session <data-file> <hex-key>`.
use ttrss_core::session;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: dump_session <data-file> <hex-key>");
    let key_hex = args.next().expect("hex key");
    let data = std::fs::read_to_string(&path).expect("read data").trim().to_string();
    let key_bytes = hex::decode(&key_hex).expect("decode key hex");
    let key: [u8; ttrss_core::crypt::KEY_LEN] =
        key_bytes.as_slice().try_into().expect("key length");
    let sess = session::from_stored(&data, Some(&key)).expect("decrypt session");
    for (k, v) in sess.items() {
        println!("{k} = {v:?}");
    }
}
