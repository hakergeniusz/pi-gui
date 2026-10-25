mod app;
mod chat;
mod rpc;
mod session_list;
mod style;

use libadwaita::Application;
use libadwaita::prelude::*;

fn main() {
    let app = Application::builder()
        .application_id("io.github.hakergeniusz.pi-gui")
        .build();

    app.connect_startup(|_app| {
        if let Err(err) = style::load_css() {
            eprintln!("failed to load css: {err}");
        }
    });

    app.connect_activate(|app| {
        let window = app::PiWindow::new(app);
        window.present();
    });

    app.run_with_args(&std::env::args().collect::<Vec<_>>());
}
