//! Spike 1: Cross-Compile-Gate. Alle nativen Deps linken, Binary startet unter Wine.
use cpal::traits::{DeviceTrait, HostTrait};
fn st(s: &str) { eprintln!("[stage] {s}"); }
fn main() {
    let skip_cam = std::env::var("SKIP_CAM").is_ok();
    let skip_audio = std::env::var("SKIP_AUDIO").is_ok();
    st("ring"); rustls::crypto::ring::default_provider().install_default().ok();
    st("rcgen"); let _kp = rcgen::KeyPair::generate().unwrap();
    st("opus"); let mut enc = opus::Encoder::new(48000, opus::Channels::Mono, opus::Application::Voip).unwrap();
    st("rnnoise"); let _dn = nnnoiseless::DenoiseState::new();
    st("openh264"); let h264 = openh264::encoder::Encoder::with_api_config(openh264::OpenH264API::from_source(), openh264::encoder::EncoderConfig::new()).is_ok();
    st("quinn"); let rt = tokio::runtime::Runtime::new().unwrap();
    let ep = rt.block_on(async { quinn::Endpoint::client("0.0.0.0:0".parse().unwrap()).map(|e| e.local_addr().unwrap()) });
    let mut dev = None; let mut cams = Err(());
    if !skip_audio { st("cpal"); let host = cpal::default_host(); dev = host.default_output_device().map(|d| d.name().unwrap_or_default()); }
    if !skip_cam { st("nokhwa"); cams = nokhwa::query(nokhwa::utils::ApiBackend::Auto).map(|c| c.len()).map_err(|_| ()); }
    println!("SPIKE-XWIN OK: opus={} out_dev={:?} cams={:?} h264={} quic_ep={:?}", enc.get_bitrate().is_ok(), dev, cams, h264, ep);
}
