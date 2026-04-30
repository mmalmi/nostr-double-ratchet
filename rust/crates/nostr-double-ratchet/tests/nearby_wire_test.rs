#![cfg(feature = "nearby")]

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use nostr_double_ratchet::{
    decode_nearby_frame_json, encode_nearby_frame_json, nearby_frame_body_len_from_header,
    read_nearby_frame,
};
use serde_json::Value;

#[test]
fn nearby_frame_round_trips_json() {
    let frame = encode_nearby_frame_json(r#"{"v":1,"type":"hello","peer_id":"abc"}"#).unwrap();
    assert_eq!(&frame[..4], b"IRIS");
    assert_eq!(
        nearby_frame_body_len_from_header(&frame[..13]),
        Some(frame.len() - 13)
    );

    let decoded = decode_nearby_frame_json(&frame).unwrap();
    let value: Value = serde_json::from_str(&decoded).unwrap();
    assert_eq!(value["type"], "hello");
    assert_eq!(value["peer_id"], "abc");
}

#[test]
fn nearby_frame_rejects_zlib_wrapped_payload() {
    let payload = br#"{"v":1,"type":"hello","peer_id":"abc"}"#;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(payload).unwrap();
    let body = encoder.finish().unwrap();

    let mut frame = Vec::new();
    frame.extend_from_slice(b"IRIS");
    frame.push(0x01);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);

    assert!(decode_nearby_frame_json(&frame).is_none());
}

#[test]
fn two_local_tcp_clients_exchange_nearby_frames_on_ephemeral_ports() {
    let alice = TcpListener::bind("127.0.0.1:0").unwrap();
    let bob = TcpListener::bind("127.0.0.1:0").unwrap();
    let alice_addr = alice.local_addr().unwrap();
    let bob_addr = bob.local_addr().unwrap();
    assert_ne!(alice_addr.port(), bob_addr.port());

    let alice_thread = thread::spawn(move || {
        let (mut socket, _) = alice.accept().unwrap();
        let frame = read_nearby_frame(&mut socket).unwrap().unwrap();
        decode_nearby_frame_json(&frame).unwrap()
    });
    let bob_thread = thread::spawn(move || {
        let (mut socket, _) = bob.accept().unwrap();
        let frame = read_nearby_frame(&mut socket).unwrap().unwrap();
        decode_nearby_frame_json(&frame).unwrap()
    });

    send_frame(
        alice_addr,
        r#"{"v":1,"type":"hello","peer_id":"bob-process"}"#,
    );
    send_frame(
        bob_addr,
        r#"{"v":1,"type":"hello","peer_id":"alice-process"}"#,
    );

    let alice_received: Value = serde_json::from_str(&alice_thread.join().unwrap()).unwrap();
    let bob_received: Value = serde_json::from_str(&bob_thread.join().unwrap()).unwrap();
    assert_eq!(alice_received["peer_id"], "bob-process");
    assert_eq!(bob_received["peer_id"], "alice-process");
}

fn send_frame(addr: std::net::SocketAddr, envelope_json: &str) {
    let frame = encode_nearby_frame_json(envelope_json).unwrap();
    let mut stream = connect_with_retry(addr);
    stream.write_all(&frame).unwrap();
}

fn connect_with_retry(addr: std::net::SocketAddr) -> TcpStream {
    for _ in 0..20 {
        if let Ok(stream) = TcpStream::connect(addr) {
            return stream;
        }
        thread::sleep(Duration::from_millis(10));
    }
    TcpStream::connect(addr).unwrap()
}
