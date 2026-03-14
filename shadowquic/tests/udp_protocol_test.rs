use bytes::Bytes;
use shadowquic::msgs::frame::{Frame, UdpAssociateReq, UdpData};
use shadowquic::msgs::socks5::SocksAddr;
use shadowquic::msgs::{SDecodeSync, SEncodeSync};

#[test]
fn test_udp_frame_roundtrip() {
    let dst = SocksAddr::from_domain("1.1.1.1".to_string(), 53);
    let req = UdpAssociateReq {
        dst: Some(dst.clone()),
        extensions: vec![],
    };

    let mut buf = bytes::BytesMut::new();
    req.encode_sync(&mut buf);

    let mut decode_buf = buf.freeze();
    let decoded = UdpAssociateReq::decode_sync(&mut decode_buf).unwrap();
    assert_eq!(decoded.dst, Some(dst));
}

#[test]
fn test_udp_data_frame_with_dst() {
    let dst = SocksAddr::from_domain("8.8.8.8".to_string(), 53);
    let payload = Bytes::from(vec![0u8; 100]);
    let udp_data = UdpData {
        dst: Some(dst.clone()),
        payload: payload.clone(),
    };

    let mut buf = bytes::BytesMut::new();
    Frame::UdpData(udp_data).encode_sync(&mut buf);

    let mut decode_buf = buf.freeze();
    let decoded = Frame::decode_sync(&mut decode_buf).unwrap();

    match decoded {
        Frame::UdpData(data) => {
            assert_eq!(data.dst, Some(dst));
            assert_eq!(data.payload.len(), 100);
        }
        _ => panic!("Expected UdpData frame"),
    }
}

#[test]
fn test_udp_data_frame_without_dst() {
    let payload = Bytes::from(vec![0u8; 50]);
    let udp_data = UdpData {
        dst: None,
        payload: payload.clone(),
    };

    let mut buf = bytes::BytesMut::new();
    Frame::UdpData(udp_data).encode_sync(&mut buf);

    let mut decode_buf = buf.freeze();
    let decoded = Frame::decode_sync(&mut decode_buf).unwrap();

    match decoded {
        Frame::UdpData(data) => {
            assert_eq!(data.dst, None);
            assert_eq!(data.payload.len(), 50);
        }
        _ => panic!("Expected UdpData frame"),
    }
}

#[tokio::test]
async fn test_udp_associate_flow() {
    let req = UdpAssociateReq {
        dst: Some(SocksAddr::from_domain("0.0.0.0".to_string(), 0)),
        extensions: vec![],
    };
    let frame = Frame::UdpAssociate(req);

    let mut buf = bytes::BytesMut::new();
    frame.encode_sync(&mut buf);

    let mut decode_buf = buf.freeze();
    let decoded = Frame::decode_sync(&mut decode_buf).unwrap();

    match decoded {
        Frame::UdpAssociate(req) => {
            assert!(req.dst.is_some());
        }
        _ => panic!("Expected UdpAssociate frame"),
    }
}

fn build_dns_query(domain: &str) -> Vec<u8> {
    let mut packet = Vec::new();

    // Transaction ID
    packet.push(0x12);
    packet.push(0x34);

    // Flags: Standard query (0x0100)
    packet.push(0x01);
    packet.push(0x00);

    // Question count: 1
    packet.push(0x00);
    packet.push(0x01);

    // Answer count: 0
    packet.push(0x00);
    packet.push(0x00);

    // Authority count: 0
    packet.push(0x00);
    packet.push(0x00);

    // Additional count: 0
    packet.push(0x00);
    packet.push(0x00);

    // Query for domain
    for label in domain.split('.') {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0x00);

    // Query type: A = 1
    packet.push(0x00);
    packet.push(0x01);

    // Query class: IN = 1
    packet.push(0x00);
    packet.push(0x01);

    packet
}

fn build_dns_response(domain: &str, ip: &str) -> Vec<u8> {
    let mut packet = Vec::new();

    // Transaction ID (echo back)
    packet.push(0x12);
    packet.push(0x34);

    // Flags: Standard response (0x8580)
    packet.push(0x85);
    packet.push(0x80);

    // Question count: 1
    packet.push(0x00);
    packet.push(0x01);

    // Answer count: 1
    packet.push(0x00);
    packet.push(0x01);

    // Authority count: 0
    packet.push(0x00);
    packet.push(0x00);

    // Additional count: 0
    packet.push(0x00);
    packet.push(0x00);

    // Query section (echo back)
    for label in domain.split('.') {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0x00);
    packet.push(0x00);
    packet.push(0x01);
    packet.push(0x00);
    packet.push(0x01);

    // Answer section
    packet.push(0xc0);
    packet.push(0x0c);
    packet.push(0x00);
    packet.push(0x01);
    packet.push(0x00);
    packet.push(0x01);
    packet.push(0x00);
    packet.push(0x00);
    packet.push(0x00);
    packet.push(0x78); // TTL: 120 seconds
    packet.push(0x00);
    packet.push(0x04); // Data length: 4

    // IP address
    for octet in ip.split('.') {
        packet.push(octet.parse().unwrap_or(0));
    }

    packet
}

#[test]
fn test_socks5_udp_dns_query_to_1_1_1_1() {
    println!("=== Testing UDP DNS Query: SOCKS5 -> ShadowQUIC -> 1.1.1.1:53 ===");

    // Step 1: Build DNS query for www.youtube.com
    let dns_query = build_dns_query("www.youtube.com");
    println!(
        "1. DNS query for www.youtube.com: {} bytes",
        dns_query.len()
    );

    // Step 2: Create UdpData frame (ShadowQUIC format)
    let dst = SocksAddr::from_domain("1.1.1.1".to_string(), 53);
    let udp_data = UdpData {
        dst: Some(dst.clone()),
        payload: Bytes::from(dns_query.clone()),
    };

    // Step 3: Encode the frame
    let mut buf = bytes::BytesMut::new();
    Frame::UdpData(udp_data).encode_sync(&mut buf);
    println!("2. Encoded UdpData frame: {} bytes", buf.len());

    // Step 4: Decode the frame (simulating server receiving)
    let mut decode_buf = buf.freeze();
    let decoded = Frame::decode_sync(&mut decode_buf).unwrap();

    match decoded {
        Frame::UdpData(data) => {
            // Verify destination
            let received_dst = data.dst.unwrap();
            println!("3. Destination: {}", received_dst);
            assert_eq!(received_dst.to_string(), "1.1.1.1:53");

            // Verify DNS query
            let payload = data.payload;
            assert!(payload.len() > 12);

            // Transaction ID
            let tid = (payload[0] as u16) << 8 | payload[1] as u16;
            println!("4. Transaction ID: 0x{:04x}", tid);

            // Flags - Query (QR=0)
            let flags = (payload[2] as u16) << 8 | payload[3] as u16;
            let is_query = (flags & 0x8000) == 0;
            assert!(is_query, "Should be a DNS query");

            // Question count
            let qdcount = (payload[4] as u16) << 8 | payload[5] as u16;
            println!("5. Question count: {}", qdcount);
            assert_eq!(qdcount, 1);

            println!("=== DNS Query test PASSED ===\n");
        }
        _ => panic!("Expected UdpData frame"),
    }
}

#[test]
fn test_socks5_udp_dns_response_from_1_1_1_1() {
    println!("=== Testing UDP DNS Response: 1.1.1.1 -> ShadowQUIC -> Client ===");

    // Step 1: Build DNS response from 1.1.1.1
    let dns_response = build_dns_response("www.youtube.com", "172.217.14.206");
    println!("1. DNS response: {} bytes", dns_response.len());

    // Step 2: Create UdpData frame
    let dst = SocksAddr::from_domain("0.0.0.0".to_string(), 0);
    let udp_data = UdpData {
        dst: Some(dst.clone()),
        payload: Bytes::from(dns_response.clone()),
    };

    // Step 3: Encode
    let mut buf = bytes::BytesMut::new();
    Frame::UdpData(udp_data).encode_sync(&mut buf);
    println!("2. Encoded UdpData frame: {} bytes", buf.len());

    // Step 4: Decode
    let mut decode_buf = buf.freeze();
    let decoded = Frame::decode_sync(&mut decode_buf).unwrap();

    match decoded {
        Frame::UdpData(data) => {
            let payload = data.payload;

            // Transaction ID
            let tid = (payload[0] as u16) << 8 | payload[1] as u16;
            println!("3. Transaction ID: 0x{:04x}", tid);
            assert_eq!(tid, 0x1234);

            // Flags - Response (QR=1)
            let flags = (payload[2] as u16) << 8 | payload[3] as u16;
            let is_response = (flags & 0x8000) != 0;
            assert!(is_response, "Should be a DNS response");

            // Answer count
            let ancount = (payload[6] as u16) << 8 | payload[7] as u16;
            println!("4. Answer count: {}", ancount);
            assert!(ancount >= 1);

            // Parse IP from answer
            // Skip to answer section: header(12) + query(domain) + 5 bytes = domain + 12 + 5
            // Simplified: start from byte 32 (typical position for A record)
            if payload.len() > 32 {
                let ip = format!(
                    "{}.{}.{}.{}",
                    payload[28], payload[29], payload[30], payload[31]
                );
                println!("5. Resolved IP: {}", ip);
            }

            println!("=== DNS Response test PASSED ===\n");
        }
        _ => panic!("Expected UdpData frame"),
    }
}

#[tokio::test]
async fn test_end_to_end_udp_dns_flow() {
    println!("=== End-to-End UDP DNS Flow Test ===\n");

    // ===== CLIENT SIDE =====
    println!("[Client] Building DNS query for www.youtube.com...");
    let dns_query = build_dns_query("www.youtube.com");

    // Wrap in UdpData frame (what ShadowQUIC sends)
    let dst = SocksAddr::from_domain("1.1.1.1".to_string(), 53);
    let udp_data = UdpData {
        dst: Some(dst.clone()),
        payload: Bytes::from(dns_query),
    };

    let mut buf = bytes::BytesMut::new();
    Frame::UdpData(udp_data).encode_sync(&mut buf);
    println!("[Client] Sending {} bytes to 1.1.1.1:53\n", buf.len());

    // ===== SERVER SIDE (receives from network) =====
    let mut decode_buf = buf.freeze();
    let decoded = Frame::decode_sync(&mut decode_buf).unwrap();

    match decoded {
        Frame::UdpData(data) => {
            println!("[Server] Received DNS query: {} bytes", data.payload.len());
            println!("[Server] Destination: {}", data.dst.as_ref().unwrap());

            // ===== SERVER SIDE (sends response back) =====
            println!("[Server] Building DNS response...");
            let dns_response = build_dns_response("www.youtube.com", "172.217.14.206");

            let response_udp_data = UdpData {
                dst: data.dst,
                payload: Bytes::from(dns_response),
            };

            let mut response_buf = bytes::BytesMut::new();
            Frame::UdpData(response_udp_data).encode_sync(&mut response_buf);
            println!(
                "[Server] Sending {} bytes back to client\n",
                response_buf.len()
            );

            // ===== CLIENT SIDE (receives response) =====
            let mut response_decode_buf = response_buf.freeze();
            let response_decoded = Frame::decode_sync(&mut response_decode_buf).unwrap();

            match response_decoded {
                Frame::UdpData(resp_data) => {
                    println!(
                        "[Client] Received DNS response: {} bytes",
                        resp_data.payload.len()
                    );

                    let payload = &resp_data.payload;
                    let flags = (payload[2] as u16) << 8 | payload[3] as u16;
                    let is_response = (flags & 0x8000) != 0;
                    assert!(is_response);

                    let ancount = (payload[6] as u16) << 8 | payload[7] as u16;
                    println!("[Client] Answers: {}", ancount);

                    println!("\n=== End-to-End UDP DNS Flow test PASSED ===");
                }
                _ => panic!("Expected UdpData frame"),
            }
        }
        _ => panic!("Expected UdpData frame"),
    }
}
