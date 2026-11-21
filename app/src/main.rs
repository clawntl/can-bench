#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod can;
mod candle;
mod device;
mod dfu;
mod dfufile;
mod periodic;
mod profile;
mod sim;
mod slcan;
mod timing;

use eframe::egui;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let shot = args.iter().position(|a| a == "--screenshot").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    if args.iter().any(|a| a == "--demo") {
        device::DEMO.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("CAN Bench – CANable terminal & firmware updater")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([900.0, 560.0]);
    if shot.is_some() {
        viewport = viewport.with_active(false);
        #[cfg(windows)]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
    }
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions { viewport, ..Default::default() };
    #[cfg(target_os = "macos")]
    if shot.is_some() {
        use winit::platform::macos::EventLoopBuilderExtMacOS;
        options.event_loop_builder = Some(Box::new(|b| {
            b.with_activate_ignoring_other_apps(false);
        }));
    }
    eframe::run_native("CAN Bench", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, shot)))))
}
