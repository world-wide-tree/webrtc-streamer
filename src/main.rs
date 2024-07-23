use std::{collections::{HashMap, HashSet}, sync::Arc, time::Duration};

use futures::StreamExt;
use opencv::{
    core::{Mat, Vector}, highgui, imgcodecs, prelude::*, videoio::{self, VideoCapture}
};
use reqwest::Client;
use reqwest_websocket::RequestBuilderExt;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt, net::{TcpListener, TcpStream}, signal, sync::{mpsc::{channel, Receiver, Sender}, Mutex, OnceCell}, task::JoinHandle};
use webrtc::{api::{interceptor_registry::register_default_interceptors, media_engine::{MediaEngine, MIME_TYPE_VP8}, APIBuilder}, ice_transport::{ice_candidate::{RTCIceCandidate, RTCIceCandidateInit}, ice_connection_state::RTCIceConnectionState, ice_server::RTCIceServer}, interceptor::registry::Registry, peer_connection::{configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription, RTCPeerConnection}, rtp_transceiver::rtp_codec::RTCRtpCodecCapability, track::track_local::{track_local_static_rtp::TrackLocalStaticRTP, TrackLocal}};
use webrtc::track::track_local::TrackLocalWriter;
#[derive(Debug, Serialize, Deserialize)]
pub struct UserIceDto{
    user_id: String,
    ice: RTCIceCandidate
}
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceIceDto{
    device_id: String,
    camera_id: String,
    ice: RTCIceCandidate
}
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceOfferDto{
    user_id: String,
    camera_id: String,
    offer: RTCSessionDescription
}
#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceAnswerDto{
    device_id: String,
    camera_id: String,
    answer: RTCSessionDescription
}
#[derive(Debug, Serialize, Deserialize)]
pub enum Cmd{
    OfferToDevice(DeviceOfferDto),
    AnswerToUser(DeviceAnswerDto),
    CandidateFromDevice(DeviceIceDto),
    CandidateFromUser(UserIceDto),
    NotSupported
}
static WEBSOCKET_URL: &str = "ws://0.0.0.0:3030/device/signaling";
static ADD_CAMERA_URL: &str = "http://0.0.0.0:3030/device/add_camera";
static ANSWER_URL: &str = "http://0.0.0.0:3030/device/answer";
static CANDIDATE_URL: &str = "http://0.0.0.0:3030/device/candidate";
static DEVICE_ID: &str = "DefaultDeviceId";

async fn signal_candidate(ice: DeviceIceDto, user_id: String){
    let msg = Cmd::CandidateFromDevice(ice);
    let response = Client::default()
        .post(format!("{}/{}", CANDIDATE_URL, user_id))
        .header("content-type", "application/json; charset=utf-8")
        .json(&msg)
        .send()
        .await
        .unwrap()
    ;
    println!("Signaled candidate to Server: {}", response.text().await.unwrap());
}
async fn signal_answer(sdp: DeviceAnswerDto, user_id: String){
    let msg = Cmd::AnswerToUser(sdp);
    let response = Client::default()
        .post(format!("{}/{}", ANSWER_URL, user_id))
        .header("content-type", "application/json; charset=utf-8")
        .json(&msg)
        .send()
        .await
        .unwrap()
    ;
    println!("Signaled answer to Server: {}", response.text().await.unwrap());
}
async fn add_camera(camera_id: String) {
    let response = Client::default()
        .post(format!("{}/{}/{}", ADD_CAMERA_URL, DEVICE_ID, camera_id))
        .send()
        .await
        .unwrap()
    ;
    println!("Added camera to Server: {}", response.text().await.unwrap());
}
struct AppState{
    pub pcs: HashMap<String, Arc<RTCPeerConnection>>,
    pub cameras: HashSet<String>,
    pub device_id: String
}
impl AppState{
    pub fn new() ->  Self{
        Self { 
            pcs: HashMap::new(),
            cameras: HashSet::new(),
            device_id: DEVICE_ID.to_string() 
        }
    }
}
static PC: OnceCell<Arc<Mutex<AppState>>> = OnceCell::const_new();
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Init camera proccess
    let mut state = AppState::new();
    state.cameras.insert("0".to_string());
    if let Err(e) = PC.set(Arc::new(Mutex::new(state))){
        println!("{e}");
    }
    let (sub_ss, sub_rr) = channel(20);
    let stream_handler = streaming_camera_task(sub_rr).await;
    if stream_handler.is_finished() {
        return stream_handler.await?.map_err(|e|e.into());
    }

    let response = Client::default()
        .get(format!("{}/{}", WEBSOCKET_URL, DEVICE_ID))
        .upgrade()
        .send()
        .await?
        ;
    let websocket = response.into_websocket().await?;
    let (sink, mut stream) = websocket.split();
    println!("Connected!");
    add_camera("0".to_string()).await;
    loop {
        println!("Awaiting Recv msg!");
        if let Some(Ok(msg)) = stream.next().await{
            println!("Received Data: {:?}", msg);
            if let Ok(msg) = msg.json::<Cmd>(){
                match msg{
                    Cmd::OfferToDevice(offer) => {
                        if PC.get().unwrap().lock().await.cameras.get(&offer.camera_id).is_some() {
                            let (frm_s, frm_r) = tokio::sync::mpsc::channel(10);
                            if let Err(e) = sub_ss.try_send(frm_s){
    
                            } else {
                                let pc = create_pc_from_offer(offer.offer, frm_r, offer.camera_id, offer.user_id.clone()).await?;
                                let mut _pc = PC.get().unwrap().lock().await;
                                _pc.pcs.insert(offer.user_id, pc);
                            }
                        }
                    },
                    Cmd::CandidateFromUser(ice) => {
                        let state = PC.get().unwrap().lock().await;
                        if let Some(pc) = state.pcs.get(&ice.user_id){
                            pc.add_ice_candidate(RTCIceCandidateInit{
                                candidate: ice.ice.to_string(),
                                ..Default::default()
                            }).await?;
                        }
                    },
                    _ => {
                        continue;
                    }
                }
            } else {
                continue;
            };
    
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Ok(())
}
async fn create_pc_from_offer(
    offer: RTCSessionDescription, 
    mut frm_r: Receiver<Vector<u8>>, 
    camera_id: String,
    user_id: String,
) -> std::result::Result<Arc<RTCPeerConnection>, Box<dyn std::error::Error>>{
    let mut m = MediaEngine::default();
    m.register_default_codecs().unwrap();
    let mut registry = Registry::new();

    // Use the default set of Interceptors
    registry = register_default_interceptors(registry, &mut m).unwrap();

    // Create the API object with the MediaEngine
    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();

    // Prepare the configuration
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };

    // Create a new RTCPeerConnection
    let peer_connection = Arc::new(api.new_peer_connection(config).await.unwrap());

    // Create Track that we send video back to browser on
    let video_track = Arc::new(TrackLocalStaticRTP::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_VP8.to_owned(),
            ..Default::default()
        },
        "video".to_owned(),
        "webrtc-rs".to_owned(),
    ));

    let rtp_sender = peer_connection
        .add_track(Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>)
        .await.unwrap();
   tokio::spawn(async move {
        let mut rtcp_buf = vec![0u8; 1500];
        while let Ok((_,_)) = rtp_sender.read(&mut rtcp_buf).await{}
   });
   let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<()>(1);
   let done_tx1 = done_tx.clone();
   peer_connection.on_ice_connection_state_change(Box::new(
        move |connection_state: RTCIceConnectionState| {
            println!("Connection State has changed {connection_state}");
            if connection_state == RTCIceConnectionState::Failed {
                let _ = done_tx1.try_send(());
            }
            Box::pin(async {})
        },
    ));
    let done_tx2 = done_tx.clone();
    peer_connection.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
        println!("Peer Connection State has changed: {s}");

        if s == RTCPeerConnectionState::Failed{
            println!("Peer Connection has gone to failed exiting: Done forwarding");
            let _ = done_tx2.try_send(());
        }

        Box::pin(async {})
    }));
    let user_id1 = user_id.clone();
    let camera_id1 = camera_id.clone();
    peer_connection.on_ice_candidate(Box::new(move |c: Option<RTCIceCandidate>| {
        let user_id2 = user_id1.clone();
        let camera_id2 = camera_id1.clone();
        Box::pin(async move {
            if let Some(c) = c{
                let ice = DeviceIceDto { 
                    device_id: DEVICE_ID.to_string(), 
                    camera_id: camera_id2, 
                    ice: c,
                };
                signal_candidate(ice, user_id2).await;
            }
        })
    }));
    println!("offer: {offer:#?}");
    peer_connection.set_remote_description(offer).await?;
    let answer = peer_connection.create_answer(None).await?;
    let mut gather_complete = peer_connection.gathering_complete_promise().await;
    peer_connection.set_local_description(answer).await?;
    let _ = gather_complete.recv().await;

    if let Some(local_desc) = peer_connection.local_description().await{
        let answer_dto = DeviceAnswerDto{
            answer: local_desc,
            device_id: DEVICE_ID.to_string(),
            camera_id
        };
        //send SDP Answer to User
        signal_answer(answer_dto, user_id).await;
    }
    let done_tx3 = done_tx.clone();
    tokio::spawn(async move {
        while let Some(n) = frm_r.recv().await{
            if let Err(e) = video_track.write(n.as_slice()).await{
                if webrtc::Error::ErrClosedPipe == e {
                    // The peerConnection has been closed.
                } else {
                    println!("video_track write err: {e}");
                }
                let _ = done_tx3.try_send(());
                return;
            }
        }
    });

    Ok(peer_connection)
}
async fn subscriber_task(mut stream: TcpStream, mut frame_r: Receiver<Vector<u8>>) -> JoinHandle<Result<(), String>> {
    let handler = tokio::spawn(async move {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n\r\n"
        );
        if let Err(e) = stream.write_all(response.as_bytes()).await {
            return Err(format!("Failed to write response: {}", e));
        }
        loop {
            if let Some(buf) = frame_r.recv().await{
                let image_data = format!(
                    "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                    buf.len()
                );
    
                if let Err(e) = stream.write_all(image_data.as_bytes()).await {
                    let msg = format!("Failed to write image data: {}", e);
                    return Err(msg);
                }
                if let Err(e) = stream.write_all(buf.as_slice()).await {
                    let msg = format!("Failed to write image buffer: {}", e);
                    return Err(msg);
                }
                if let Err(e) = stream.write_all(b"\r\n").await {
                    let msg = format!("Failed to write image end: {}", e);
                    return Err(msg);
                }
                if let Err(e) = stream.flush().await {
                    let msg = format!("Failed to flush stream: {}", e);
                    return Err(msg);
                }
            }
            // tokio::select! {
            //     _ = signal::ctrl_c() => {
            //         break;
            //     }
            // }
        }
        Ok(())
    });
    handler
}

async fn streaming_camera_task(mut subscribe: Receiver<Sender<Vector<u8>>>) -> JoinHandle<Result<(), String>>{
    let h: JoinHandle<Result<(), String>> = tokio::spawn(async move {
        let mut cap = VideoCapture::new_def(0).map_err(|e| e.to_string())?;//from_file_def("rtsp://admin:123qazwsx@192.168.1.64:554/Streaming/channels/102").map_err(|e| e.to_string())?;
        if !cap.is_opened().map_err(|e| {eprintln!("{e}");e.to_string()})?{
            println!("Camera not opened");
            return Err("VideoCapture not opened!".into());
        } else {
            println!("Camera opened!");
        }
        let mut subscribers = Vec::new();
        loop {
            let mut buf = Vector::new();
            let mut frame = Mat::default();
            
            if !cap.read(&mut frame).map_err(|e| e.to_string())? || frame.size().map_err(|e| e.to_string())?.width <= 0 {
                return Err("Failed to read frame".into());
            }
            
            let _ = imgcodecs::imencode_def(".jpg", &frame, &mut buf);
            
            if let Ok(subscriber) = subscribe.try_recv(){
                subscribers.push(subscriber);
            }
            subscribers.retain(|subscriber|{
                if let Err(e) = subscriber.try_send(buf.clone()){
                    eprintln!("Frame send error: {e}. Close channel.");
                    false
                } else {
                    true
                }
            });
            // subscribers.iter_mut().for_each(|subscriber| {
            //     if subscriber.1{
            //         if let Err(e) = subscriber.0.try_send(buf.clone()){
            //             subscriber.1 = false;
            //             eprintln!("Frame send error: {e}");
            //         }
            //     } else {
            //         subscriber.
            //     }
            // });
            // highgui::imshow("ServerWindow", &frame).map_err(|e| e.to_string())?;
            // highgui::wait_key(1).map_err(|e| e.to_string())?;
            tokio::time::sleep(Duration::from_millis(30)).await;
            // tokio::select! {
            //     _ = signal::ctrl_c() => {
            //         break;
            //     }
            // }
        }
        Ok(())
    });
    h
}