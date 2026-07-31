#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod backend;
mod child_process;
mod model;
mod theme;

use app::CodeAgentApp;
use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CodeAgent")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([980.0, 640.0])
            .with_icon(app_icon()),
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "CodeAgent",
        options,
        Box::new(|cc| Ok(Box::new(CodeAgentApp::new(cc)))),
    )
}

fn app_icon() -> egui::IconData {
    let size = 64usize;
    let mut rgba = vec![0_u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let i = (y * size + x) * 4;
            let dx = x as f32 - 31.5;
            let dy = y as f32 - 31.5;
            let rounded = dx.abs().max(dy.abs()) < 27.0
                || (dx.abs() - 27.0).max(0.0).powi(2) + (dy.abs() - 27.0).max(0.0).powi(2) < 20.0;
            if rounded {
                rgba[i..i + 4].copy_from_slice(&[16, 18, 24, 255]);
                let ring = (dx * dx + dy * dy).sqrt();
                if (18.0..=22.0).contains(&ring) {
                    rgba[i..i + 4].copy_from_slice(&[127, 91, 255, 255]);
                }
                if dx.abs() < 3.0 && dy.abs() < 14.0 {
                    rgba[i..i + 4].copy_from_slice(&[244, 242, 255, 255]);
                }
                if dy.abs() < 3.0 && dx.abs() < 14.0 {
                    rgba[i..i + 4].copy_from_slice(&[244, 242, 255, 255]);
                }
            }
        }
    }
    egui::IconData {
        rgba,
        width: size as u32,
        height: size as u32,
    }
}
