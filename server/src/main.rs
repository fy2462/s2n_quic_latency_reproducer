use hbb_common::{
    message_proto::{message, Message as FrameMessage},
    protobuf::Message,
    quic::{server, Connection, ReceiveAPI, SendAPI},
    tokio, ResultType,
};
use std::time;

#[tokio::main()]
async fn main() -> ResultType<()> {
    let bind_addr = format!("{}:{}", "0.0.0.0", 50011)
        .parse()
        .expect("parse signaling server address error");
    let mut server_endpoint = server::new_server(bind_addr).expect("create server error");
    println!("Start server demo");
    loop {
        tokio::select! {
            Some(mut new_conn) = server_endpoint.accept() => {
                let client_addr = new_conn.remote_addr().unwrap();
                tokio::spawn(async move {
                    println!("Found new connection, client addr: {:?}", &client_addr);
                    while let Ok(Some(stream)) = new_conn.accept_bidirectional_stream().await {
                        println!("Accept new bidirectional stream");
                        let mut conn = Connection::new_conn_wrapper(stream).await.unwrap();
                        tokio::spawn(async move {
                            loop {
                                if let Some(result) = conn.next().await {
                                    match result {
                                        Err(err) => {
                                            println!("read msg for peer {:?} with error: {:?}", &client_addr, err);
                                            break;
                                        }
                                        Ok(bytes) => {
                                            if let Ok(msg_in) = FrameMessage::parse_from_bytes(&bytes) {
                                                match msg_in.union {
                                                    Some(message::Union::VideoFrame(vf)) => {
                                                        if vf.timestamp > 0 {
                                                            let data = bytes.freeze();
                                                            println!("forward the bytes received, timestamp: {:?}, size: {:?}", time::Instant::now(), data.len());
                                                            conn.send_bytes(data).await.ok();
                                                        }
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        });
                    }
                });
            }
        }
    }
}
