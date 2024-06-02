use std::{sync::Arc, time::Duration};

use bytes::Bytes;
// use hyper::{Method, Request};
use lazy_static::lazy_static;
use opencv::{core::{self, Vector}, highgui::{self, wait_key}, imgcodecs::imencode_def, prelude::*, videoio::{self, VideoCapture}};
use reqwest::{Body, Client, Request};
use tokio::sync::{broadcast::channel, mpsc, Mutex};
use webrtc::{api::{interceptor_registry::register_default_interceptors, media_engine::MediaEngine, APIBuilder}, data_channel::{data_channel_message::DataChannelMessage, RTCDataChannel}, ice_transport::{ice_candidate::RTCIceCandidate, ice_server::RTCIceServer}, interceptor::registry::Registry, peer_connection::{configuration::RTCConfiguration, math_rand_alpha, offer_answer_options::RTCAnswerOptions, peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription, RTCPeerConnection}};


lazy_static! {
    static ref PEER_CONNECTION_MUTEX: Arc<Mutex<Option<Arc<RTCPeerConnection>>>> =
        Arc::new(Mutex::new(None));
    static ref PENDING_CANDIDATES: Arc<Mutex<Vec<RTCIceCandidate>>> = Arc::new(Mutex::new(vec![]));
    static ref ADDRESS: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>>{
    let clnt = Client::new();

    let (ts, mut tr) = channel(100);
    let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);

    tokio::spawn(async move {
        let mut cap = VideoCapture::new_def(0).unwrap();
        loop {
            let mut frm = Mat::default();

            if let Err(e) = cap.read(&mut frm){
                eprintln!("Cannot read frame!");
                continue;
            }
            
            if let Err(e) = highgui::imshow("winname", &frm){
                eprintln!("Cannot show frame!");
                continue;
            }
            if let Err(e) = wait_key(1){
                eprintln!("Cannot wait key!");
                continue;
            }

            let mut buf = Vector::new();
            if let Err(e) = imencode_def(".jpg", &frm, &mut buf){
                continue;
            }
            if let Err(e) = ts.send(buf){
                
            }
        }
    });

    // WebRTC connection setting up
    let config = RTCConfiguration{
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut media = MediaEngine::default();
    media.register_default_codecs()?;

    let mut registry = Registry::new();
    registry = register_default_interceptors(registry, &mut media)?;


    // Create the API object with the MediaEngine
    let api = APIBuilder::new()
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .build();

    let peer_connection = api.new_peer_connection(config).await?;

    peer_connection.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
        println!("Peer Connection State has changed: {s}");

        if s == RTCPeerConnectionState::Failed {
            // Wait until PeerConnection has had no network activity for 30 seconds or another failure. It may be reconnected using an ICE Restart.
            // Use webrtc.PeerConnectionStateDisconnected if you are interested in detecting faster timeout.
            // Note that the PeerConnection may come back from PeerConnectionStateDisconnected.
            println!("Peer Connection has gone to failed exiting");
            let _ = done_tx.try_send(());
        }

        Box::pin(async {})
    }));
    println!("safdsdf");
    let offer = loop {
        let r = clnt.get("http://0.0.0.0:3030/signal/0").send().await?;
        if r.status().is_success(){
            let sdp: RTCSessionDescription = r.json().await?;
            break sdp;
        } else {
            println!("1");
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    };

    peer_connection.set_remote_description(offer).await?;
    // Create Answer
    let answer = peer_connection.create_answer(None).await?;
    println!("safdsdf");

    // Sent SDP to SignalService
    let r = clnt.post("http://0.0.0.0:3030/answer/0").body(serde_json::to_string(&answer)?).send().await?;


    peer_connection.set_local_description(answer).await?;

    let dc = peer_connection.create_data_channel("VideoStreamer", None).await?;

    loop {
        if let Ok(frm_buf) = tr.try_recv(){
            let data = Bytes::from(frm_buf.to_vec());
            if let Err(e) = dc.send(&data).await{
                println!("{e}");
            }
        }

    }
    tokio::select! {
        _ = done_rx.recv() => {
            println!("received done signal!");
        }
        _ = tokio::signal::ctrl_c() => {
            println!();
        }
    };
    peer_connection.close().await?;

    Ok(())
    
}



