use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use opencv::{core::{Mat_, Vector}, imgcodecs::{imencode, imencode_def}, imgproc, prelude::*, videoio};
use serde::{Deserialize, Serialize};
use tokio::{sync::mpsc, time::interval};
use webrtc::{api::{media_engine::MediaEngine, APIBuilder}, ice_transport::ice_candidate::RTCIceCandidateInit, media::Sample, peer_connection::{configuration::RTCConfiguration, sdp::session_description::RTCSessionDescription}, rtp_transceiver::rtp_codec::RTCRtpCodecCapability, track::track_local::track_local_static_sample::TrackLocalStaticSample};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut media_engine = MediaEngine::default();
    media_engine.register_default_codecs()?;

    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .build()
    ;

    // Конфигурация PeerConnection
    let config = RTCConfiguration::default();
    let peer_connection = Arc::new(api.new_peer_connection(config).await?);

    // Создание видеотрека
    let video_track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: "video/vp8".to_owned(),
            ..Default::default()
        },
        "video".to_owned(),
        "webrtc-rs".to_owned(),
    ));
    peer_connection.add_track(video_track.clone()).await?;

    // Настройка канала для передачи кадров
    let (tx, mut rx) = mpsc::channel(100);

    tokio::spawn(async move {
        // Захват видео с камеры
        let mut cam = videoio::VideoCapture::new(0, videoio::CAP_ANY).unwrap();
        let opened = videoio::VideoCapture::is_opened(&cam).unwrap();
        if !opened {
            panic!("Unable to open default camera!");
        }

        let mut frame = Mat::default();
        while videoio::VideoCapture::read(&mut cam, &mut frame).unwrap() {
            if frame.empty() {
                continue;
            }
            // Преобразование кадра в формат, подходящий для передачи
            let mut yuv_frame = Mat::default();
            imgproc::cvt_color(&frame, &mut yuv_frame, imgproc::COLOR_BGR2YUV_I420, 0).unwrap();

            let sample_data = yuv_frame.data_bytes().unwrap().to_vec();
            
            let sample = Sample{
                data: Bytes::from(sample_data),
                duration: Duration::from_secs(1) / 30, //30 fps
                ..Default::default()
            };
            if let Err(e) = tx.try_send(sample){
                eprintln!("Error: {e}");
            }
        }
    });
    tokio::spawn(async move {
        // Настройка сигнального сервера
        let client = reqwest::Client::new();
        let mut interval = interval(Duration::from_secs(1));

        loop {
            interval.tick().await;

            let sdp = peer_connection.local_description().await.unwrap();
            let message = SignalMessage {
                sdp: Some(sdp),
                candidate: None,
            };

            let response = client.post("http://0.0.0.0:3030/signal")
                .json(&message)
                .send()
                .await.unwrap()
                .json::<SignalMessage>()
                .await.unwrap();

            if let Some(remote_sdp) = response.sdp {
                peer_connection.set_remote_description(remote_sdp).await.unwrap();
            }

            if let Some(candidate) = response.candidate {
                peer_connection.add_ice_candidate(candidate).await.unwrap();
            }
        }
    });
    // Обработка кадров и отправка их в видеотрек
    while let Some(sample) = rx.recv().await {
        video_track.write_sample(&sample).await.unwrap();
    }

    println!("Hello, world!");
    Ok(())
}
#[derive(Serialize, Deserialize, Clone)]
struct SignalMessage {
    sdp: Option<RTCSessionDescription>,
    candidate: Option<RTCIceCandidateInit>,
}