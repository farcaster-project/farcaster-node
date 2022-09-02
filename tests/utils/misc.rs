//! Miscellaneous helper functions.

use farcaster_node::bus::Request;
use internet2::transport::MAX_FRAME_SIZE;
use internet2::Decrypt;
use internet2::PlainTranscoder;
use internet2::RoutedFrame;
use internet2::{CreateUnmarshaller, Unmarshall};

pub fn get_request_from_message(message: Vec<Vec<u8>>) -> Request {
    // Receive a Request
    let unmarshaller = Request::create_unmarshaller();
    let mut transcoder = PlainTranscoder {};
    let routed_message = recv_routed(message);
    let plain_message = transcoder.decrypt(routed_message.msg).unwrap();
    (&*unmarshaller.unmarshall(&*plain_message).unwrap()).clone()
}

// as taken from the rust-internet2 crate - for now we only use the message
// field, but there is value in parsing all for visibiliy and testing routing
// information
pub fn recv_routed(message: std::vec::Vec<std::vec::Vec<u8>>) -> RoutedFrame {
    let mut multipart = message.into_iter();
    // Skipping previous hop data since we do not need them
    let hop = multipart.next().unwrap();
    let src = multipart.next().unwrap();
    let dst = multipart.next().unwrap();
    let msg = multipart.next().unwrap();
    if multipart.count() > 0 {
        panic!("multipart message empty");
    }
    let len = msg.len();
    if len > MAX_FRAME_SIZE as usize {
        panic!(
            "multipart message frame
size too big"
        );
    }
    RoutedFrame { hop, src, dst, msg }
}
