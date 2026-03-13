use bytes::Bytes;
use shadowquic::msgs::frame::{
    ClientHello, ConnectReq, ErrorFrame, Frame, FrameType, ServerHello, UdpAssociateReq,
    UdpData, ERROR_OK, FEATURE_UDP,
};
use shadowquic::msgs::socks5::SocksAddr;
use shadowquic::msgs::{SDecodeSync, SEncodeSync};

#[test]
fn test_udp_frame_roundtrip() {
    // Test UDP Associate frame encoding/decoding
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
    // Test UdpData frame with destination address
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
    // Test UdpData frame without destination address (subsequent packets)
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
    use shadowquic::msgs::SEncode;
    
    // Simulate UDP associate flow
    // 1. Client sends UdpAssociate request
    let req = UdpAssociateReq {
        dst: Some(SocksAddr::from_domain("0.0.0.0".to_string(), 0)),
        extensions: vec![],
    };
    let frame = Frame::UdpAssociate(req);
    
    // 2. Encode the frame
    let mut buf = bytes::BytesMut::new();
    frame.encode_sync(&mut buf);
    
    // 3. Decode the frame
    let mut decode_buf = buf.freeze();
    let decoded = Frame::decode_sync(&mut decode_buf).unwrap();
    
    match decoded {
        Frame::UdpAssociate(req) => {
            assert!(req.dst.is_some());
        }
        _ => panic!("Expected UdpAssociate frame"),
    }
}
