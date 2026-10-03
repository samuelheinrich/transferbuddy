mod app;
mod design;
mod instance;
mod native;
mod preferences;
use clap::Parser;
use std::{path::PathBuf, sync::mpsc};
use transferbuddy_core::{config::Config, VERSION};
#[derive(Parser)]
#[command(version=VERSION,about="TransferBuddy desktop — shared Rust engine with the terminal app")]
struct Options {
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long)]
    config: Option<PathBuf>,
}
fn main() -> anyhow::Result<()> {
    let opts = Options::parse();
    let dir = opts
        .config
        .as_ref()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(Config::default_dir);
    let (tx, rx) = mpsc::channel();
    let ctx = eframe::egui::Context::default();
    let Some(_instance) = instance::Instance::acquire(&dir, ctx.clone(), tx.clone())? else {
        return Ok(());
    };
    native::install_port_broker();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let prefs = preferences::Preferences::load(&dir);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(format!("TransferBuddy {VERSION}"))
            .with_icon(eframe::egui::IconData {
                rgba: design::icon_rgba(64),
                width: 64,
                height: 64,
            })
            .with_inner_size(prefs.size)
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    let handle = runtime.handle().clone();
    eframe::run_native_ext(
        "TransferBuddy",
        options,
        Some(ctx),
        Box::new(move |cc| {
            Ok(Box::new(app::Desktop::new(
                &cc.egui_ctx,
                handle,
                opts.config,
                opts.root,
                tx,
                rx,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    runtime.shutdown_timeout(std::time::Duration::from_secs(3));
    Ok(())
}
